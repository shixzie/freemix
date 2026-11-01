//! The as-run log (OR-015): every realized Program change the daemon writes
//! beside the project, one tab-separated line per change.
//!
//! ```text
//! 1761860000000    1500    7    Camera 1
//! ```
//!
//! The first field is the wall-clock Unix time in milliseconds, the second
//! the realized engine frame, the third the input id, and the last its name
//! (tabs and newlines in a name become spaces). The file is appended to, so
//! a restart keeps the show's earlier run; it is not part of `project.json`
//! and a restore never rewrites it.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fm_engine::EngineAsRunEntry;

/// The as-run log's name inside a project bundle.
pub(crate) const AS_RUN_FILE: &str = "as-run.log";

/// The daemon's as-run log: the last entry written, so each pass appends only
/// what the engine recorded since.
pub(crate) struct AsRunLog {
    path: PathBuf,
    /// The last entry written, to resume after the engine's bounded buffer
    /// drops its oldest entries.
    last: Option<(u64, fm_types::InputId)>,
}

impl AsRunLog {
    /// The log beside the bundle at `bundle`.
    pub(crate) fn new(bundle: &Path) -> Self {
        Self {
            path: bundle.join(AS_RUN_FILE),
            last: None,
        }
    }

    /// Appends every entry the engine recorded since the last call, with one
    /// wall-clock stamp for the pass.
    pub(crate) fn sync(&mut self, entries: &[EngineAsRunEntry]) {
        let start = match self.last {
            None => 0,
            Some(last) => entries
                .iter()
                .position(|entry| (entry.frame, entry.input) == last)
                .map_or(0, |index| index + 1),
        };
        if start >= entries.len() {
            return;
        }
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        for entry in &entries[start..] {
            let name = entry.name.replace(['\t', '\n', '\r'], " ");
            let _ = writeln!(file, "{now}\t{}\t{}\t{name}", entry.frame, entry.input);
            self.last = Some((entry.frame, entry.input));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU128;

    fn entry(frame: u64, input: u128, name: &str) -> EngineAsRunEntry {
        EngineAsRunEntry {
            frame,
            input: fm_types::InputId::new(NonZeroU128::new(input).unwrap()),
            name: name.to_owned(),
        }
    }

    #[test]
    fn the_log_appends_each_change_once() {
        let directory = tempfile::tempdir().unwrap();
        let mut log = AsRunLog::new(directory.path());
        let entries = [entry(10, 1, "Camera 1"), entry(20, 2, "Slides\t2")];
        log.sync(&entries);
        log.sync(&entries);
        log.sync(&entries[..1]);
        let text = std::fs::read_to_string(directory.path().join(AS_RUN_FILE)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].ends_with("\t10\t1\tCamera 1"), "{text}");
        assert!(lines[1].ends_with("\t20\t2\tSlides 2"), "{text}");
    }

    #[test]
    fn a_dropped_entry_restarts_the_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let mut log = AsRunLog::new(directory.path());
        log.sync(&[entry(1, 1, "A"), entry(2, 2, "B")]);
        // The engine's buffer dropped the last entry; the remaining ones are
        // appended again from the start.
        log.sync(&[entry(3, 3, "C")]);
        let text = std::fs::read_to_string(directory.path().join(AS_RUN_FILE)).unwrap();
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(text.ends_with("\t3\t3\tC\n"), "{text}");
    }
}
