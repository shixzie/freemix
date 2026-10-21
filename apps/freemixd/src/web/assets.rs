//! The project's image assets as the browser title editor sees them
//! (GX-006): `GET /v1/assets/images` lists the PNG and JPEG pictures of the
//! bundle's `assets/` tree, `GET /v1/assets/image?uri=asset://…` serves one
//! (the picker's thumbnails), and `POST /v1/assets/images?name=…` stores a
//! new picture under `assets/uploads/`.
//!
//! Every request presents a bearer token whose identity may edit the
//! project: the operator's `FREEMIXD_WEB_TOKEN`, or a paired device whose
//! role carries `edit_project` (Graphics). A Viewer, Audio, or Operator
//! device is refused with 403, a missing, unknown, or revoked token with
//! 401, and a browser request from another origin with 403.
//!
//! An upload is held to [`MAX_UPLOAD_BYTES`] (refused from its
//! `Content-Length` before any body byte is read), must declare
//! `image/png` or `image/jpeg` and carry that format's signature, and must
//! decode completely within [`UPLOAD_DECODE_LIMITS`]. It is stored under a
//! name the server makes, `<stem>-<16 hex of its SHA-256>.<png|jpg>`: the
//! client's name contributes only a sanitized stem (`[a-z0-9_-]`, at most
//! 40 bytes), never a separator, a dot segment, or the extension. The same
//! picture uploaded again is answered with the stored one. The upload
//! directory holds at most [`MAX_UPLOADED_IMAGES`] pictures, must be a real
//! directory inside the canonical assets root, and is written through a
//! synced temporary file linked into place without replacing anything.

use std::{
    fs::{self, File},
    io::{self, Read, Write as _},
    num::NonZeroU128,
    path::{Path, PathBuf},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use fm_auth::{DeviceRole, Permission, Policy};
use fm_codec_image::{StillDecodeLimits, decode_still};
use fm_frame::{
    ClockDomainId, MediaTimestamp, MediaTiming, NormalizedDuration, NormalizedTimestamp,
    OriginalTimestamp, SequenceNumber, TimeBase,
};

use super::{
    WebAccess, WebIdentity,
    http::{self, Request, json_response, json_string},
};

/// The asset list and upload path.
pub(super) const IMAGES_PATH: &str = "/v1/assets/images";
/// The single-picture path (`?uri=asset://…`).
pub(super) const IMAGE_PATH: &str = "/v1/assets/image";
/// The largest picture uploaded or served, in bytes.
pub(super) const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;
/// The most pictures `assets/uploads/` holds.
pub(super) const MAX_UPLOADED_IMAGES: usize = 256;
/// How long reading an upload's body, or writing a served picture, may take.
pub(super) const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
/// Uploads read, decoded, and stored at once; a third is told to retry.
const MAX_CONCURRENT_UPLOADS: usize = 2;
/// The directory, under the assets root, uploads are stored in.
const UPLOAD_DIRECTORY: &str = "uploads";
/// The deepest directory the listing descends into below the assets root.
const MAX_LIST_DEPTH: usize = 6;
/// The most directory entries one listing looks at.
const MAX_LIST_VISITS: usize = 4096;
/// The most pictures one listing names.
const MAX_LISTED_IMAGES: usize = 512;
/// The longest sanitized stem an uploaded file name keeps.
const MAX_STEM_BYTES: usize = 40;

/// Decoding bounds an upload must meet: the bounds a title draws an image
/// within (`fm_titles::MAX_OUTPUT_WIDTH` by `MAX_OUTPUT_HEIGHT`, and at most
/// `fm_titles::MAX_IMAGE_ASSET_BYTES` decoded, 16 MiB of RGBA: 2048 x 2048,
/// or 3840 x 1092). A larger picture would be stored and chosen, then never
/// drawn, so it is refused here instead.
pub(super) const UPLOAD_DECODE_LIMITS: StillDecodeLimits = StillDecodeLimits {
    max_encoded_bytes: MAX_UPLOAD_BYTES,
    max_width: fm_titles::MAX_OUTPUT_WIDTH,
    max_height: fm_titles::MAX_OUTPUT_HEIGHT,
    max_decoded_rgba_bytes: fm_titles::MAX_IMAGE_ASSET_BYTES,
    max_icc_bytes: 1024 * 1024,
    max_image_alloc_bytes: 512 * 1024 * 1024,
};

static UPLOADS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
/// Serializes the count check and the write of concurrent uploads.
static STORE: Mutex<()> = Mutex::new(());

/// A picture format the editor accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ImageKind {
    Png,
    Jpeg,
}

impl ImageKind {
    const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }

    const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }

    /// The format a `Content-Type` value declares (parameters ignored).
    fn declared(content_type: &str) -> Option<Self> {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match mime.as_str() {
            "image/png" => Some(Self::Png),
            "image/jpeg" => Some(Self::Jpeg),
            _ => None,
        }
    }

    /// The format `bytes` carry the signature of.
    fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else {
            None
        }
    }
}

fn error(status: &str, code: &str) -> Vec<u8> {
    json_response(status, &format!(r#"{{"error":"{code}"}}"#), "")
}

fn unauthorized() -> Vec<u8> {
    error("401 Unauthorized", "unauthorized")
}

fn forbidden() -> Vec<u8> {
    error("403 Forbidden", "forbidden")
}

pub(super) fn not_found() -> Vec<u8> {
    error("404 Not Found", "not_found")
}

pub(super) fn unavailable() -> Vec<u8> {
    error("503 Service Unavailable", "unavailable")
}

fn storage_failed() -> Vec<u8> {
    error("500 Internal Server Error", "storage")
}

/// Whether a paired device of `role` may edit the project.
fn role_may_edit(role: DeviceRole) -> bool {
    let policy = Policy::production();
    role.granted_roles().iter().any(|granted| {
        policy
            .permissions_for(*granted)
            .is_some_and(|permissions| permissions.contains(&Permission::EditProject))
    })
}

/// Admits a request whose bearer token may edit the project; otherwise the
/// refusal to answer with.
pub(super) fn authorize(request: &Request, tls: bool, access: &WebAccess) -> Result<(), Vec<u8>> {
    if !http::same_origin(request, tls, false) {
        return Err(forbidden());
    }
    let identity = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| access.authenticate(token.as_bytes()))
        .ok_or_else(unauthorized)?;
    match &identity {
        WebIdentity::StaticToken => Ok(()),
        WebIdentity::Device { .. } => {
            // The registry's current role, so a revoked device is refused.
            let (_, role) = access.describe(&identity).ok_or_else(unauthorized)?;
            if role_may_edit(role) {
                Ok(())
            } else {
                Err(forbidden())
            }
        }
    }
}

fn has_image_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && ["png", "jpg", "jpeg"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// Every listed picture: `(asset key, bytes)`, sorted by key, at most
/// [`MAX_LISTED_IMAGES`]. Symbolic links are never followed.
fn list_images(assets_root: &Path) -> Vec<(String, u64)> {
    let Ok(root) = fs::canonicalize(assets_root) else {
        return Vec::new();
    };
    let mut images = Vec::new();
    let mut directories = vec![(root, String::new(), 0_usize)];
    let mut visits = 0;
    while let Some((directory, prefix, depth)) = directories.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            visits += 1;
            if visits > MAX_LIST_VISITS {
                directories.clear();
                break;
            }
            let Ok(entry) = entry else { continue };
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            let key = format!("{prefix}{name}");
            if metadata.is_dir() {
                if depth < MAX_LIST_DEPTH {
                    directories.push((entry.path(), format!("{key}/"), depth + 1));
                }
            } else if metadata.is_file()
                && has_image_extension(name)
                && fm_persistence::validate_asset_uri(&format!("asset://{key}")).is_ok()
            {
                images.push((key, metadata.len()));
            }
        }
    }
    images.sort();
    images.truncate(MAX_LISTED_IMAGES);
    images
}

/// The pictures in the upload directory.
fn uploaded_count(assets_root: &Path) -> usize {
    fs::read_dir(assets_root.join(UPLOAD_DIRECTORY)).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| !name.starts_with('.') && has_image_extension(name))
                    && fs::symlink_metadata(entry.path()).is_ok_and(|metadata| metadata.is_file())
            })
            .count()
    })
}

/// `GET /v1/assets/images`.
pub(super) fn list_response(assets_root: &Path) -> Vec<u8> {
    let images = list_images(assets_root)
        .iter()
        .map(|(key, bytes)| {
            format!(
                r#"{{"uri":{},"bytes":{bytes}}}"#,
                json_string(&format!("asset://{key}"))
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    json_response(
        "200 OK",
        &format!(
            r#"{{"images":[{images}],"max_bytes":{MAX_UPLOAD_BYTES},"max_count":{MAX_UPLOADED_IMAGES},"uploaded":{},"types":["image/png","image/jpeg"]}}"#,
            uploaded_count(assets_root)
        ),
        "",
    )
}

/// `GET /v1/assets/image?uri=asset://<key>`: the picture, when the URI is
/// strict, resolves inside the canonical assets root to a regular file of
/// at most [`MAX_UPLOAD_BYTES`], and carries a PNG or JPEG signature.
pub(super) fn image_response(assets_root: &Path, request: &Request, body: bool) -> Vec<u8> {
    let Some(uri) = request.query_param("uri") else {
        return not_found();
    };
    if fm_persistence::validate_asset_uri(&uri).is_err() {
        return not_found();
    }
    let Some(key) = uri.strip_prefix("asset://") else {
        return not_found();
    };
    let Ok(root) = fs::canonicalize(assets_root) else {
        return not_found();
    };
    let Ok(path) = fs::canonicalize(root.join(key)) else {
        return not_found();
    };
    if !path.starts_with(&root) {
        return not_found();
    }
    // Only a regular file is opened: opening a FIFO or a device in the
    // bundle could block this worker indefinitely.
    if !fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
        return not_found();
    }
    let Ok(file) = File::open(&path) else {
        return not_found();
    };
    match file.metadata() {
        Ok(metadata) if metadata.is_file() => {
            if metadata.len() > MAX_UPLOAD_BYTES as u64 {
                return error("413 Content Too Large", "too_large");
            }
        }
        _ => return not_found(),
    }
    let mut bytes = Vec::new();
    if file
        .take(MAX_UPLOAD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return not_found();
    }
    if bytes.len() > MAX_UPLOAD_BYTES {
        return error("413 Content Too Large", "too_large");
    }
    let Some(kind) = ImageKind::sniff(&bytes) else {
        return not_found();
    };
    let mut response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: {}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'none'\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\n\
         Referrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n",
        kind.mime(),
        bytes.len()
    )
    .into_bytes();
    if body {
        response.extend_from_slice(&bytes);
    }
    response
}

/// One of the [`MAX_CONCURRENT_UPLOADS`] upload slots, released on drop.
pub(super) struct UploadPermit(());

impl UploadPermit {
    pub(super) fn acquire() -> Option<Self> {
        let mut held = UPLOADS_IN_FLIGHT.load(Ordering::Acquire);
        while held < MAX_CONCURRENT_UPLOADS {
            match UPLOADS_IN_FLIGHT.compare_exchange_weak(
                held,
                held + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self(())),
                Err(current) => held = current,
            }
        }
        None
    }
}

impl Drop for UploadPermit {
    fn drop(&mut self) {
        UPLOADS_IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Checks an upload's head before any body byte is read: the declared
/// format and the body length to read.
pub(super) fn upload_head(request: &Request) -> Result<(ImageKind, usize), Vec<u8>> {
    // The body is read by its `Content-Length` alone. A framed body
    // (`Transfer-Encoding`, which overrides the length) would be stored with
    // its chunk framing, so it is refused rather than misread.
    if request.has_header("transfer-encoding") {
        return Err(error("400 Bad Request", "bad_request"));
    }
    let Some(length) = request.header("content-length") else {
        return Err(error("411 Length Required", "length_required"));
    };
    let Some(length) = length
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| length.parse::<usize>().ok())
        .flatten()
    else {
        return Err(error("400 Bad Request", "bad_request"));
    };
    if length > MAX_UPLOAD_BYTES {
        return Err(error("413 Content Too Large", "too_large"));
    }
    if length == 0 {
        return Err(error("422 Unprocessable Content", "invalid_image"));
    }
    let kind = request
        .header("content-type")
        .and_then(ImageKind::declared)
        .ok_or_else(|| error("415 Unsupported Media Type", "unsupported_type"))?;
    Ok((kind, length))
}

/// The file stem a client's `name` contributes: its last path component's
/// stem, lowercased, every run of characters outside `[a-z0-9_]` one `-`,
/// trimmed, at most [`MAX_STEM_BYTES`]; `image` when nothing is left.
fn safe_stem(name: Option<&str>) -> String {
    let base = name
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let stem = base.rsplit_once('.').map_or(base, |(stem, _)| stem);
    let mut safe = String::new();
    for character in stem.chars() {
        let character = character.to_ascii_lowercase();
        if character.is_ascii_alphanumeric() || character == '_' {
            safe.push(character);
        } else if !safe.is_empty() && !safe.ends_with('-') {
            safe.push('-');
        }
        if safe.len() >= MAX_STEM_BYTES {
            break;
        }
    }
    safe.truncate(MAX_STEM_BYTES);
    let safe = safe.trim_matches('-');
    if safe.is_empty() {
        "image".to_owned()
    } else {
        safe.to_owned()
    }
}

fn media_timing() -> Option<MediaTiming> {
    let time_base = TimeBase::new(1, 1_000_000_000).ok()?;
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(0), time_base),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(1).ok()?,
        ClockDomainId::new(NonZeroU128::MIN),
        SequenceNumber::new(0),
    )
    .ok()
}

/// The canonical upload directory, created when missing; `None` when it
/// is a link, not a directory, or resolves outside the assets root.
fn upload_directory(assets_root: &Path) -> Option<PathBuf> {
    real_directory(assets_root)?;
    let root = fs::canonicalize(assets_root).ok()?;
    let directory = root.join(UPLOAD_DIRECTORY);
    real_directory(&directory)?;
    let canonical = fs::canonicalize(&directory).ok()?;
    (canonical == directory && canonical.starts_with(&root)).then_some(canonical)
}

/// Ensures `path` is a directory itself (not a link to one), creating it
/// when missing.
fn real_directory(path: &Path) -> Option<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => metadata.is_dir().then_some(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(path).ok(),
        Err(_) => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// A stored file in `directory` holding exactly `bytes` whose name ends in
/// `suffix`.
fn stored_copy(directory: &Path, suffix: &str, bytes: &[u8]) -> Option<String> {
    let entries = fs::read_dir(directory).ok()?;
    entries.filter_map(Result::ok).find_map(|entry| {
        let name = entry.file_name().to_str()?.to_owned();
        if name.starts_with('.') || !name.ends_with(suffix) {
            return None;
        }
        let metadata = fs::symlink_metadata(entry.path()).ok()?;
        (metadata.is_file()
            && metadata.len() == bytes.len() as u64
            && fs::read(entry.path()).ok()? == bytes)
            .then_some(name)
    })
}

fn stored_response(status: &str, name: &str, bytes: usize, width: u32, height: u32) -> Vec<u8> {
    json_response(
        status,
        &format!(
            r#"{{"uri":{},"bytes":{bytes},"width":{width},"height":{height}}}"#,
            json_string(&format!("asset://{UPLOAD_DIRECTORY}/{name}"))
        ),
        "",
    )
}

/// Validates and stores one uploaded picture; the answer to send.
pub(super) fn store_upload(
    assets_root: &Path,
    name: Option<&str>,
    declared: ImageKind,
    bytes: &[u8],
) -> Vec<u8> {
    if ImageKind::sniff(bytes) != Some(declared) {
        return error("415 Unsupported Media Type", "unsupported_type");
    }
    let Some(timing) = media_timing() else {
        return storage_failed();
    };
    let Ok(decoded) = decode_still(bytes, timing, UPLOAD_DECODE_LIMITS) else {
        return error("422 Unprocessable Content", "invalid_image");
    };
    let (width, height) = (
        decoded.source_dimensions.width(),
        decoded.source_dimensions.height(),
    );
    drop(decoded);

    let _store = STORE.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(directory) = upload_directory(assets_root) else {
        return storage_failed();
    };
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let suffix = format!("-{}.{}", hex(&digest.as_ref()[..8]), declared.extension());
    if let Some(existing) = stored_copy(&directory, &suffix, bytes) {
        return stored_response("200 OK", &existing, bytes.len(), width, height);
    }
    if uploaded_count(assets_root) >= MAX_UPLOADED_IMAGES {
        return error("409 Conflict", "limit");
    }
    let file_name = format!("{}{suffix}", safe_stem(name));
    let target = directory.join(&file_name);
    let mut random = [0_u8; 8];
    if getrandom::fill(&mut random).is_err() {
        return storage_failed();
    }
    let temporary = directory.join(format!(".upload-{}.tmp", hex(&random)));
    let written = File::options()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        });
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
        return storage_failed();
    }
    // A hard link never replaces an existing name.
    let linked = fs::hard_link(&temporary, &target);
    let _ = fs::remove_file(&temporary);
    match linked {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return if fs::read(&target).is_ok_and(|stored| stored == bytes) {
                stored_response("200 OK", &file_name, bytes.len(), width, height)
            } else {
                self::error("409 Conflict", "exists")
            };
        }
        Err(_) => return storage_failed(),
    }
    #[cfg(unix)]
    if let Ok(directory) = File::open(&directory) {
        let _ = directory.sync_all();
    }
    stored_response("201 Created", &file_name, bytes.len(), width, height)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::net::{IpAddr, Ipv4Addr};

    use fm_protocol::{WebAccessAction, WebDeviceRole};

    use super::{
        ImageKind, MAX_UPLOAD_BYTES, MAX_UPLOADED_IMAGES, UploadPermit, authorize, image_response,
        list_response, safe_stem, store_upload, upload_head,
    };
    use crate::web::{WebAccess, http::Request};

    const TOKEN: &[u8] = b"assets-test-token-0123456789abcdef";

    fn png(width: u32, height: u32, seed: u8) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([
                seed,
                u8::try_from(x % 256).unwrap(),
                u8::try_from(y % 256).unwrap(),
                255,
            ])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn jpeg() -> Vec<u8> {
        let image = image::RgbImage::from_fn(16, 8, |x, _| {
            image::Rgb([u8::try_from(x * 8).unwrap(), 0, 0])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .unwrap();
        bytes
    }

    fn request(head: &str) -> Request {
        Request::parse(format!("{head}\r\n\r\n").as_bytes()).unwrap()
    }

    fn status(response: &[u8]) -> String {
        let text = String::from_utf8_lossy(response);
        text.split("\r\n").next().unwrap().to_owned()
    }

    fn body(response: &[u8]) -> String {
        let text = String::from_utf8_lossy(response).into_owned();
        text.split_once("\r\n\r\n").unwrap().1.to_owned()
    }

    fn get_image(root: &std::path::Path, uri: &str) -> Vec<u8> {
        image_response(
            root,
            &request(&format!(
                "GET /v1/assets/image?uri={uri} HTTP/1.1\r\nHost: a"
            )),
            true,
        )
    }

    #[test]
    fn only_editing_identities_are_admitted() {
        let directory = tempfile::tempdir().unwrap();
        let access = WebAccess::open(
            "https://192.168.1.20:8443".into(),
            None,
            Some(TOKEN.to_vec()),
            Some(directory.path()),
        )
        .unwrap();
        let device = |role| {
            let (_, status) = access.handle(&WebAccessAction::Pair { role });
            let code = status.pairing.unwrap().code;
            access
                .redeem(&code, "Phone", IpAddr::V4(Ipv4Addr::LOCALHOST))
                .unwrap()
        };
        let head = |token: &str, origin: &str| {
            request(&format!(
                "GET /v1/assets/images HTTP/1.1\r\nHost: 192.168.1.20:8443\r\nAuthorization: Bearer {token}{origin}"
            ))
        };
        let token = String::from_utf8(TOKEN.to_vec()).unwrap();
        assert!(authorize(&head(&token, ""), true, &access).is_ok());
        let graphics = device(WebDeviceRole::Graphics);
        assert!(authorize(&head(&graphics.token, ""), true, &access).is_ok());
        for role in [
            WebDeviceRole::Viewer,
            WebDeviceRole::Audio,
            WebDeviceRole::Operator,
        ] {
            let refused = authorize(&head(&device(role).token, ""), true, &access).unwrap_err();
            assert_eq!(status(&refused), "HTTP/1.1 403 Forbidden", "{role:?}");
        }
        let refused = authorize(&head("fmd1_nope", ""), true, &access).unwrap_err();
        assert_eq!(status(&refused), "HTTP/1.1 401 Unauthorized");
        let bare = request("GET /v1/assets/images HTTP/1.1\r\nHost: 192.168.1.20:8443");
        assert_eq!(
            status(&authorize(&bare, true, &access).unwrap_err()),
            "HTTP/1.1 401 Unauthorized"
        );
        // Another page's origin is refused even with a good token.
        let foreign = head(&token, "\r\nOrigin: https://evil.example");
        assert_eq!(
            status(&authorize(&foreign, true, &access).unwrap_err()),
            "HTTP/1.1 403 Forbidden"
        );
        let same = head(&token, "\r\nOrigin: https://192.168.1.20:8443");
        assert!(authorize(&same, true, &access).is_ok());
        // A revoked Graphics device is no longer anyone.
        access.handle(&WebAccessAction::Revoke {
            device: graphics.id.clone(),
        });
        assert_eq!(
            status(&authorize(&head(&graphics.token, ""), true, &access).unwrap_err()),
            "HTTP/1.1 401 Unauthorized"
        );
    }

    #[test]
    fn uploads_are_validated_stored_once_and_listed() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("assets");
        let picture = png(32, 16, 7);
        let stored = store_upload(
            &root,
            Some("Team Logo (final).PNG"),
            ImageKind::Png,
            &picture,
        );
        assert_eq!(status(&stored), "HTTP/1.1 201 Created");
        let answer = body(&stored);
        assert!(
            answer.starts_with(r#"{"uri":"asset://uploads/team-logo-final-"#),
            "{answer}"
        );
        assert!(answer.ends_with(&format!(
            r#".png","bytes":{},"width":32,"height":16}}"#,
            picture.len()
        )));
        let uri = answer.split('"').nth(3).unwrap().to_owned();
        let file = uri.strip_prefix("asset://").unwrap();
        assert_eq!(std::fs::read(root.join(file)).unwrap(), picture);
        // Again, under any name: the stored copy answers and nothing is written.
        let again = store_upload(&root, Some("other.png"), ImageKind::Png, &picture);
        assert_eq!(status(&again), "HTTP/1.1 200 OK");
        assert!(body(&again).contains(&uri));
        assert_eq!(std::fs::read_dir(root.join("uploads")).unwrap().count(), 1);

        let photo = jpeg();
        let stored = store_upload(&root, None, ImageKind::Jpeg, &photo);
        assert_eq!(status(&stored), "HTTP/1.1 201 Created");
        assert!(body(&stored).starts_with(r#"{"uri":"asset://uploads/image-"#));
        assert!(body(&stored).contains(r#".jpg","#));

        // Other pictures, hidden files, and non-pictures in the bundle.
        std::fs::create_dir_all(root.join("images/deep")).unwrap();
        std::fs::write(root.join("images/deep/b.JPEG"), &photo).unwrap();
        std::fs::write(root.join("images/.hidden.png"), &picture).unwrap();
        std::fs::write(root.join("images/notes.txt"), b"x").unwrap();
        std::fs::write(root.join("images/bad name%.png"), &picture).unwrap();
        let listed = body(&list_response(&root));
        assert!(listed.starts_with(&format!(
            r#"{{"images":[{{"uri":"asset://images/deep/b.JPEG","bytes":{}}},"#,
            photo.len()
        )));
        assert!(listed.contains(&format!(r#"{{"uri":"{uri}","bytes":{}}}"#, picture.len())));
        assert!(!listed.contains("hidden") && !listed.contains("notes") && !listed.contains('%'));
        assert!(listed.ends_with(&format!(
            r#""max_bytes":{MAX_UPLOAD_BYTES},"max_count":{MAX_UPLOADED_IMAGES},"uploaded":2,"types":["image/png","image/jpeg"]}}"#
        )));

        // The listed picture is served with its sniffed type.
        let served = get_image(&root, &uri.replace("://", "%3A%2F%2F"));
        assert_eq!(status(&served), "HTTP/1.1 200 OK");
        let text = String::from_utf8_lossy(&served);
        assert!(text.contains("Content-Type: image/png\r\n"));
        assert!(text.contains("X-Content-Type-Options: nosniff\r\n"));
        assert!(text.contains("Content-Security-Policy: default-src 'none'\r\n"));
        assert!(served.ends_with(&picture));
    }

    #[test]
    fn uploads_that_are_not_what_they_claim_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("assets");
        let picture = png(8, 8, 1);
        // PNG bytes declared as JPEG, and the reverse.
        assert_eq!(
            status(&store_upload(&root, None, ImageKind::Jpeg, &picture)),
            "HTTP/1.1 415 Unsupported Media Type"
        );
        assert_eq!(
            status(&store_upload(&root, None, ImageKind::Png, &jpeg())),
            "HTTP/1.1 415 Unsupported Media Type"
        );
        // Markup claiming to be a picture.
        for markup in [
            &b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>"[..],
            b"<!doctype html><script>alert(1)</script>",
        ] {
            assert_eq!(
                status(&store_upload(&root, None, ImageKind::Png, markup)),
                "HTTP/1.1 415 Unsupported Media Type"
            );
        }
        // A signature followed by garbage, and a truncated picture.
        let mut forged = b"\x89PNG\r\n\x1a\n".to_vec();
        forged.extend_from_slice(&[0x55; 200]);
        assert_eq!(
            status(&store_upload(&root, None, ImageKind::Png, &forged)),
            "HTTP/1.1 422 Unprocessable Content"
        );
        let truncated = &picture[..picture.len() / 2];
        assert_eq!(
            status(&store_upload(&root, None, ImageKind::Png, truncated)),
            "HTTP/1.1 422 Unprocessable Content"
        );
        assert!(
            !root.join("uploads").exists()
                || std::fs::read_dir(root.join("uploads")).unwrap().count() == 0
        );
    }

    #[test]
    fn upload_heads_are_checked_before_the_body() {
        let head = |headers: &str| {
            upload_head(&request(&format!(
                "POST /v1/assets/images?name=a.png HTTP/1.1\r\nHost: a{headers}"
            )))
        };
        assert_eq!(
            status(&head("\r\nContent-Type: image/png").unwrap_err()),
            "HTTP/1.1 411 Length Required"
        );
        assert_eq!(
            status(
                &head(&format!(
                    "\r\nContent-Type: image/png\r\nContent-Length: {}",
                    MAX_UPLOAD_BYTES + 1
                ))
                .unwrap_err()
            ),
            "HTTP/1.1 413 Content Too Large"
        );
        assert_eq!(
            status(&head("\r\nContent-Type: image/png\r\nContent-Length: +5").unwrap_err()),
            "HTTP/1.1 400 Bad Request"
        );
        for kind in ["image/svg+xml", "text/html", "image/gif"] {
            assert_eq!(
                status(
                    &head(&format!("\r\nContent-Type: {kind}\r\nContent-Length: 10")).unwrap_err()
                ),
                "HTTP/1.1 415 Unsupported Media Type"
            );
        }
        assert_eq!(
            head("\r\nContent-Type: IMAGE/JPEG; q=1\r\nContent-Length: 10").unwrap(),
            (ImageKind::Jpeg, 10)
        );
        // A chunked body is never read by its length.
        for framing in [
            "\r\nTransfer-Encoding: chunked",
            "\r\nTransfer-Encoding: chunked\r\nContent-Length: 10",
            "\r\nTransfer-Encoding: identity\r\nContent-Length: 10",
        ] {
            assert_eq!(
                status(&head(&format!("\r\nContent-Type: image/png{framing}")).unwrap_err()),
                "HTTP/1.1 400 Bad Request",
                "{framing}"
            );
        }
    }

    #[test]
    fn uploads_a_title_could_never_draw_are_refused() {
        // 4097 x 1024 RGBA is just over the 16 MiB a title decodes an image
        // into (fm_titles::MAX_IMAGE_ASSET_BYTES); storing it would hand
        // the editor an image that renders blank on air.
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("assets");
        let image = image::GrayImage::new(4097, 1024);
        let mut wide = Vec::new();
        image::DynamicImage::ImageLuma8(image)
            .write_to(&mut Cursor::new(&mut wide), image::ImageFormat::Png)
            .unwrap();
        assert!(wide.len() < MAX_UPLOAD_BYTES);
        assert_eq!(
            status(&store_upload(
                &root,
                Some("wide.png"),
                ImageKind::Png,
                &wide
            )),
            "HTTP/1.1 422 Unprocessable Content"
        );
        assert_eq!(super::UPLOAD_DECODE_LIMITS.max_decoded_rgba_bytes, 16 << 20);
        assert!(
            !root.join("uploads").exists()
                || std::fs::read_dir(root.join("uploads")).unwrap().count() == 0
        );
    }

    #[test]
    fn upload_names_never_leave_the_upload_directory() {
        for (name, stem) in [
            ("../../etc/passwd.png", "passwd"),
            ("..\\..\\windows\\evil.png", "evil"),
            ("a/b/../c.png", "c"),
            ("..", "image"),
            (".png", "image"),
            ("x\0y.png", "x-y"),
            ("Ünïcode ñame.jpeg", "n-code-ame"),
            ("", "image"),
            ("logo.png.exe", "logo-png"),
        ] {
            assert_eq!(safe_stem(Some(name)), stem, "{name:?}");
        }
        assert_eq!(safe_stem(Some(&"a".repeat(200))).len(), 40);

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("assets");
        let picture = png(4, 4, 9);
        let stored = store_upload(
            &root,
            Some("../../../project.json"),
            ImageKind::Png,
            &picture,
        );
        assert_eq!(status(&stored), "HTTP/1.1 201 Created");
        assert!(body(&stored).starts_with(r#"{"uri":"asset://uploads/project-"#));
        assert!(!directory.path().join("project.json").exists());
        // Only one entry anywhere: the stored picture in uploads/.
        let entries: Vec<_> = std::fs::read_dir(root.join("uploads"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn served_uris_stay_inside_the_bundle() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("assets");
        std::fs::create_dir_all(root.join("uploads")).unwrap();
        let picture = png(4, 4, 3);
        std::fs::write(directory.path().join("outside.png"), &picture).unwrap();
        std::fs::write(directory.path().join("project.json"), b"{}").unwrap();
        std::fs::write(root.join("notes.png"), b"<script>alert(1)</script>").unwrap();
        for uri in [
            "asset://../project.json",
            "asset%3A%2F%2F..%2Foutside.png",
            "asset://uploads/../../outside.png",
            "asset://uploads/..%2F..%2Foutside.png",
            "asset:///etc/passwd",
            "asset://notes.png",
            "asset://missing.png",
            "file:///etc/passwd",
            "",
        ] {
            assert_eq!(
                status(&get_image(&root, uri)),
                "HTTP/1.1 404 Not Found",
                "{uri}"
            );
        }
        assert_eq!(
            status(&image_response(
                &root,
                &request("GET /v1/assets/image HTTP/1.1\r\nHost: a"),
                true
            )),
            "HTTP/1.1 404 Not Found"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(directory.path().join("outside.png"), root.join("link.png"))
                .unwrap();
            assert_eq!(
                status(&get_image(&root, "asset://link.png")),
                "HTTP/1.1 404 Not Found"
            );
            // Listing never follows the link either.
            assert!(!body(&list_response(&root)).contains("link.png"));
            // A FIFO is answered at once, never opened (opening one would
            // wait for a writer forever).
            let fifo = root.join("pipe.png");
            if std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .is_ok_and(|status| status.success())
            {
                assert_eq!(
                    status(&get_image(&root, "asset://pipe.png")),
                    "HTTP/1.1 404 Not Found"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_upload_directory_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("assets");
        let elsewhere = directory.path().join("elsewhere");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("uploads")).unwrap();
        let stored = store_upload(&root, Some("a.png"), ImageKind::Png, &png(4, 4, 2));
        assert_eq!(status(&stored), "HTTP/1.1 500 Internal Server Error");
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn the_upload_directory_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("assets");
        std::fs::create_dir_all(root.join("uploads")).unwrap();
        for index in 0..MAX_UPLOADED_IMAGES {
            std::fs::write(root.join(format!("uploads/p{index}.png")), b"x").unwrap();
        }
        let refused = store_upload(&root, None, ImageKind::Png, &png(4, 4, 5));
        assert_eq!(status(&refused), "HTTP/1.1 409 Conflict");
        assert_eq!(body(&refused), r#"{"error":"limit"}"#);
    }

    #[test]
    fn concurrent_uploads_are_bounded() {
        let first = UploadPermit::acquire();
        let second = UploadPermit::acquire();
        // Other tests may hold a permit; at most two are ever granted.
        let third = UploadPermit::acquire();
        assert!(third.is_none() || first.is_none() || second.is_none());
        drop((first, second, third));
    }
}
