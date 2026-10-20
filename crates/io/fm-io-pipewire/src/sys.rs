//! The libpipewire functions and structures the stream reader uses,
//! loaded at run time.
//!
//! Only plain exported functions are called (no inline header helpers
//! dispatching through SPA interfaces), and only the leading fields of the
//! structures below are read, all stable since `PipeWire` 0.3. The library is
//! opened once per process and never closed: `PipeWire` keeps threads and
//! plugin state that outlive any one stream.

#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

use libloading::Library;

/// The shared library loaded, by its ABI name.
pub const LIBRARY: &str = "libpipewire-0.3.so.0";

/// `struct spa_dict_item`.
#[repr(C)]
pub struct SpaDictItem {
    pub key: *const c_char,
    pub value: *const c_char,
}

/// `struct spa_dict`.
#[repr(C)]
pub struct SpaDict {
    pub flags: u32,
    pub n_items: u32,
    pub items: *const SpaDictItem,
}

/// `struct spa_hook`: a listener's list link, callbacks, and private
/// fields. Zeroed before use and pinned while registered.
#[repr(C)]
pub struct SpaHook {
    link: [*mut c_void; 2],
    funcs: *const c_void,
    data: *mut c_void,
    removed: *mut c_void,
    private: *mut c_void,
}

impl SpaHook {
    /// An unregistered hook.
    pub const fn zeroed() -> Self {
        Self {
            link: [std::ptr::null_mut(); 2],
            funcs: std::ptr::null(),
            data: std::ptr::null_mut(),
            removed: std::ptr::null_mut(),
            private: std::ptr::null_mut(),
        }
    }
}

/// `struct spa_chunk`: the valid region of one data plane.
#[repr(C)]
pub struct SpaChunk {
    pub offset: u32,
    pub size: u32,
    pub stride: i32,
    pub flags: i32,
}

/// `struct spa_data`: one data plane of a buffer.
#[repr(C)]
pub struct SpaData {
    pub type_: u32,
    pub flags: u32,
    pub fd: i64,
    pub mapoffset: u32,
    pub maxsize: u32,
    pub data: *mut c_void,
    pub chunk: *mut SpaChunk,
}

/// `struct spa_buffer`.
#[repr(C)]
pub struct SpaBuffer {
    pub n_metas: u32,
    pub n_datas: u32,
    pub metas: *mut c_void,
    pub datas: *mut SpaData,
}

/// The leading field of `struct pw_buffer` (only read through a pointer
/// the stream hands out).
#[repr(C)]
pub struct PwBuffer {
    pub buffer: *mut SpaBuffer,
}

/// `PW_VERSION_STREAM_EVENTS`.
pub const STREAM_EVENTS_VERSION: u32 = 2;

/// `struct pw_stream_events`.
#[repr(C)]
pub struct StreamEvents {
    pub version: u32,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    pub control_info: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    pub io_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32)>,
    pub param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    pub add_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    pub remove_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    pub process: Option<unsafe extern "C" fn(*mut c_void)>,
    pub drained: Option<unsafe extern "C" fn(*mut c_void)>,
    pub command: Option<unsafe extern "C" fn(*mut c_void, *const c_void)>,
    pub trigger_done: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// `PW_VERSION_PROXY_EVENTS`.
pub const PROXY_EVENTS_VERSION: u32 = 1;

/// `struct pw_proxy_events`.
#[repr(C)]
pub struct ProxyEvents {
    pub version: u32,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub bound: Option<unsafe extern "C" fn(*mut c_void, u32)>,
    pub removed: Option<unsafe extern "C" fn(*mut c_void)>,
    pub done: Option<unsafe extern "C" fn(*mut c_void, c_int)>,
    pub error: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    pub bound_props: Option<unsafe extern "C" fn(*mut c_void, u32, *const SpaDict)>,
}

/// `enum pw_stream_state` values.
pub const STATE_ERROR: c_int = -1;
pub const STATE_UNCONNECTED: c_int = 0;
pub const STATE_PAUSED: c_int = 2;
pub const STATE_STREAMING: c_int = 3;
/// `SPA_DIRECTION_INPUT`.
pub const DIRECTION_INPUT: c_int = 0;
/// `PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS |
/// PW_STREAM_FLAG_DONT_RECONNECT`: linked to the shared node and nothing
/// else, so a node that goes away fails the stream ("target not found")
/// instead of the session manager linking it to another video source.
pub const FLAGS_AUTOCONNECT_MAP_BUFFERS_DONT_RECONNECT: c_int = (1 << 0) | (1 << 2) | (1 << 7);
/// `SPA_CHUNK_FLAG_CORRUPTED`.
pub const CHUNK_CORRUPTED: i32 = 1;

/// Opaque libpipewire objects.
pub type Opaque = c_void;

/// The functions used, copied out of the library, which stays loaded for
/// the life of the process.
#[allow(clippy::struct_field_names)]
pub struct Api {
    _library: Library,
    pub init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
    pub thread_loop_new: unsafe extern "C" fn(*const c_char, *const SpaDict) -> *mut Opaque,
    pub thread_loop_get_loop: unsafe extern "C" fn(*mut Opaque) -> *mut Opaque,
    pub thread_loop_start: unsafe extern "C" fn(*mut Opaque) -> c_int,
    pub thread_loop_stop: unsafe extern "C" fn(*mut Opaque),
    pub thread_loop_lock: unsafe extern "C" fn(*mut Opaque),
    pub thread_loop_unlock: unsafe extern "C" fn(*mut Opaque),
    pub thread_loop_destroy: unsafe extern "C" fn(*mut Opaque),
    pub context_new: unsafe extern "C" fn(*mut Opaque, *mut Opaque, usize) -> *mut Opaque,
    pub context_destroy: unsafe extern "C" fn(*mut Opaque),
    pub context_connect_fd:
        unsafe extern "C" fn(*mut Opaque, c_int, *mut Opaque, usize) -> *mut Opaque,
    pub core_disconnect: unsafe extern "C" fn(*mut Opaque) -> c_int,
    pub properties_new_dict: unsafe extern "C" fn(*const SpaDict) -> *mut Opaque,
    pub proxy_add_listener:
        unsafe extern "C" fn(*mut Opaque, *mut SpaHook, *const ProxyEvents, *mut c_void),
    pub stream_new: unsafe extern "C" fn(*mut Opaque, *const c_char, *mut Opaque) -> *mut Opaque,
    pub stream_add_listener:
        unsafe extern "C" fn(*mut Opaque, *mut SpaHook, *const StreamEvents, *mut c_void),
    pub stream_connect:
        unsafe extern "C" fn(*mut Opaque, c_int, u32, c_int, *mut *const c_void, u32) -> c_int,
    pub stream_update_params: unsafe extern "C" fn(*mut Opaque, *mut *const c_void, u32) -> c_int,
    pub stream_disconnect: unsafe extern "C" fn(*mut Opaque) -> c_int,
    pub stream_destroy: unsafe extern "C" fn(*mut Opaque),
    pub stream_dequeue_buffer: unsafe extern "C" fn(*mut Opaque) -> *mut PwBuffer,
    pub stream_queue_buffer: unsafe extern "C" fn(*mut Opaque, *mut PwBuffer) -> c_int,
}

/// Why libpipewire cannot be used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Unavailable {
    /// The library is not installed (or not on the loader's path).
    NotInstalled(String),
    /// The library lacks a function used here (a `PipeWire` too old).
    Unusable(String),
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled(detail) => write!(formatter, "{LIBRARY} is not installed: {detail}"),
            Self::Unusable(detail) => write!(formatter, "{LIBRARY} cannot be used: {detail}"),
        }
    }
}

/// Copies the function `name` (NUL-terminated) out of `library`.
macro_rules! function {
    ($library:expr, $name:literal) => {{
        // SAFETY: the symbol is a libpipewire function whose C signature
        // is the field's type (checked against the PipeWire 1.0 headers);
        // the copied pointer stays valid because the library is kept
        // loaded for the life of the process.
        let symbol = unsafe { $library.get($name) };
        *symbol.map_err(|error| {
            Unavailable::Unusable(format!(
                "{}: {error}",
                String::from_utf8_lossy(&$name[..$name.len() - 1])
            ))
        })?
    }};
}

/// Loads the library `name` and its functions.
pub fn load(name: &str) -> Result<Api, Unavailable> {
    // SAFETY: loading libpipewire runs only its ELF initializers, which
    // set up nothing but static data; it is never unloaded.
    let library = unsafe { Library::new(name) }
        .map_err(|error| Unavailable::NotInstalled(error.to_string()))?;
    Ok(Api {
        init: function!(library, b"pw_init\0"),
        thread_loop_new: function!(library, b"pw_thread_loop_new\0"),
        thread_loop_get_loop: function!(library, b"pw_thread_loop_get_loop\0"),
        thread_loop_start: function!(library, b"pw_thread_loop_start\0"),
        thread_loop_stop: function!(library, b"pw_thread_loop_stop\0"),
        thread_loop_lock: function!(library, b"pw_thread_loop_lock\0"),
        thread_loop_unlock: function!(library, b"pw_thread_loop_unlock\0"),
        thread_loop_destroy: function!(library, b"pw_thread_loop_destroy\0"),
        context_new: function!(library, b"pw_context_new\0"),
        context_destroy: function!(library, b"pw_context_destroy\0"),
        context_connect_fd: function!(library, b"pw_context_connect_fd\0"),
        core_disconnect: function!(library, b"pw_core_disconnect\0"),
        properties_new_dict: function!(library, b"pw_properties_new_dict\0"),
        proxy_add_listener: function!(library, b"pw_proxy_add_listener\0"),
        stream_new: function!(library, b"pw_stream_new\0"),
        stream_add_listener: function!(library, b"pw_stream_add_listener\0"),
        stream_connect: function!(library, b"pw_stream_connect\0"),
        stream_update_params: function!(library, b"pw_stream_update_params\0"),
        stream_disconnect: function!(library, b"pw_stream_disconnect\0"),
        stream_destroy: function!(library, b"pw_stream_destroy\0"),
        stream_dequeue_buffer: function!(library, b"pw_stream_dequeue_buffer\0"),
        stream_queue_buffer: function!(library, b"pw_stream_queue_buffer\0"),
        _library: library,
    })
}

/// The process's libpipewire, loaded and initialized (`pw_init`) on first
/// use.
pub fn api() -> Result<&'static Api, Unavailable> {
    static API: OnceLock<Result<Api, Unavailable>> = OnceLock::new();
    API.get_or_init(|| {
        let api = load(LIBRARY)?;
        // SAFETY: `pw_init` accepts null arguments, and runs once per
        // process here.
        unsafe { (api.init)(std::ptr::null_mut(), std::ptr::null_mut()) };
        Ok(api)
    })
    .as_ref()
    .map_err(Clone::clone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_library_is_not_installed() {
        let Err(Unavailable::NotInstalled(detail)) = load("libfreemix-no-such-library.so.0") else {
            panic!("loaded a library that does not exist");
        };
        assert!(detail.contains("libfreemix-no-such-library"), "{detail}");
    }

    #[test]
    fn a_library_without_pipewire_is_unusable() {
        // The C library is always loaded, and has none of these functions.
        let Err(Unavailable::Unusable(detail)) = load("libc.so.6") else {
            panic!("libc passed for libpipewire");
        };
        assert!(detail.starts_with("pw_init"), "{detail}");
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn structures_have_their_c_layout() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<SpaHook>(), 48);
        assert_eq!(size_of::<SpaChunk>(), 16);
        assert_eq!(offset_of!(SpaData, fd), 8);
        assert_eq!(offset_of!(SpaData, data), 24);
        assert_eq!(offset_of!(SpaData, chunk), 32);
        assert_eq!(size_of::<SpaData>(), 40);
        assert_eq!(offset_of!(SpaBuffer, datas), 16);
        assert_eq!(offset_of!(StreamEvents, process), 64);
        assert_eq!(offset_of!(ProxyEvents, error), 40);
    }
}
