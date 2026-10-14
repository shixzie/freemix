//! Durable file primitives shared by the bundle operations.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use crate::store::{TempGuard, next_temp_sequence, parent_directory, sync_directory};

/// Builds a hidden temporary sibling of `path` that is unique per process.
pub(crate) fn temp_sibling(path: &Path, tag: &str) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |name| name.to_string_lossy().into_owned());
    parent_directory(path).join(format!(
        ".{name}.{tag}-{}-{}",
        std::process::id(),
        next_temp_sequence()
    ))
}

/// Most fresh temporary names tried before giving up.
const MAX_TEMP_ATTEMPTS: u32 = 64;

/// Exclusively creates a fresh temporary sibling of `path`.
///
/// A leftover temporary file from an earlier process that had the same id
/// (common for long-lived daemons restarted in containers) is skipped rather
/// than reported as an error or deleted. The returned guard removes only the
/// file this call created.
fn create_temp_sibling(path: &Path, tag: &str) -> io::Result<(PathBuf, File, TempGuard)> {
    let mut attempts = 0;
    loop {
        let temp = temp_sibling(path, tag);
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => return Ok((temp.clone(), file, TempGuard::new(temp))),
            // Exhaustion is not reported as `AlreadyExists`, which callers
            // read as "the destination is occupied".
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                attempts += 1;
                if attempts == MAX_TEMP_ATTEMPTS {
                    return Err(io::Error::other("no free temporary file name"));
                }
            }
            Err(error) => return Err(error),
        }
    }
}

/// Atomically replaces `path` with `bytes`.
///
/// The bytes are written to a temporary sibling, flushed with `sync_all`, then
/// renamed into place and the parent directory is synced. A failure leaves the
/// previous file, if any, untouched.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (temp, mut file, mut guard) = create_temp_sibling(path, "tmp")?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, path)?;
    guard.disarm();
    sync_directory(parent_directory(path)).map_err(store_io)
}

/// Copies the regular file `source` to `destination` without ever replacing
/// an existing destination.
///
/// The bytes land in a synced temporary sibling first and are then published
/// with a hard link, which fails atomically when the destination exists. When
/// the filesystem does not support hard links the temporary file is renamed
/// after an existence check instead. Returns the number of bytes copied, or an
/// [`io::ErrorKind::AlreadyExists`] error when `destination` is occupied.
pub(crate) fn copy_new_file(source: &Path, destination: &Path) -> io::Result<u64> {
    let mut input = File::open(source)?;
    if !input.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "copy source is not a regular file",
        ));
    }
    let (temp, mut output, _guard) = create_temp_sibling(destination, "copy")?;
    let bytes = io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    drop(output);
    match fs::hard_link(&temp, destination) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(error),
        Err(_) => {
            if fs::symlink_metadata(destination).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "copy destination already exists",
                ));
            }
            fs::rename(&temp, destination)?;
        }
    }
    sync_directory(parent_directory(destination)).map_err(store_io)?;
    Ok(bytes)
}

/// Reports whether two files hold byte-identical contents.
pub(crate) fn same_contents(left: &Path, right: &Path) -> io::Result<bool> {
    let mut left = File::open(left)?;
    let mut right = File::open(right)?;
    if left.metadata()?.len() != right.metadata()?.len() {
        return Ok(false);
    }
    let mut left_buffer = vec![0_u8; 64 * 1024];
    let mut right_buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = read_full(&mut left, &mut left_buffer)?;
        if read == 0 {
            return Ok(read_full(&mut right, &mut right_buffer[..1])? == 0);
        }
        if read_full(&mut right, &mut right_buffer[..read])? != read
            || left_buffer[..read] != right_buffer[..read]
        {
            return Ok(false);
        }
    }
}

fn read_full(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// Creates `relative`'s parent directories beneath `root` and proves the
/// result still lies inside `root` once symlinks are resolved.
pub(crate) fn create_parents_within(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let destination = root.join(relative);
    let parent = parent_directory(&destination).to_path_buf();
    fs::create_dir_all(root)?;
    let canonical_root = fs::canonicalize(root)?;
    let escapes = |path: &Path| -> io::Result<bool> {
        Ok(!fs::canonicalize(path)?.starts_with(&canonical_root))
    };
    // Check the deepest existing ancestor before creating anything, so a
    // symlinked directory cannot make us create directories elsewhere.
    let mut existing = parent.as_path();
    while fs::symlink_metadata(existing).is_err() {
        existing = parent_directory(existing);
    }
    if escapes(existing)? {
        return Err(outside_bundle());
    }
    fs::create_dir_all(&parent)?;
    if escapes(&parent)? {
        return Err(outside_bundle());
    }
    Ok(destination)
}

fn outside_bundle() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "destination directory resolves outside the bundle",
    )
}

fn store_io(error: crate::StoreError) -> io::Error {
    match error {
        crate::StoreError::Io(error) => error,
        other => io::Error::other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "fm-persistence-fsutil-{}-{}-{name}",
                std::process::id(),
                next_temp_sequence()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Simulates leftovers from an earlier process with this process id by
    /// occupying the next temporary names.
    fn squat_next_temp_names(path: &Path, tag: &str) -> Vec<PathBuf> {
        let next = next_temp_sequence() + 1;
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        (next..next + 32)
            .map(|sequence| {
                let squatter = parent_directory(path)
                    .join(format!(".{name}.{tag}-{}-{sequence}", std::process::id()));
                fs::write(&squatter, b"stale").unwrap();
                squatter
            })
            .collect()
    }

    #[test]
    fn atomic_writes_skip_leftover_temporary_files() {
        let temp = TestDirectory::new("write-squatted");
        let path = temp.0.join("recent.json");
        fs::write(&path, b"old").unwrap();
        let squatters = squat_next_temp_names(&path, "tmp");

        write_atomically(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(squatters.iter().all(|squatter| squatter.exists()));
    }

    #[test]
    fn copies_skip_leftover_temporary_files() {
        let temp = TestDirectory::new("copy-squatted");
        let source = temp.0.join("source.mov");
        fs::write(&source, b"media").unwrap();
        let destination = temp.0.join("clip.mov");
        let squatters = squat_next_temp_names(&destination, "copy");

        assert_eq!(copy_new_file(&source, &destination).unwrap(), 5);
        assert_eq!(fs::read(&destination).unwrap(), b"media");
        assert!(squatters.iter().all(|squatter| squatter.exists()));
    }
}
