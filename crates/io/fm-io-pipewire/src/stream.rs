//! One raw video input stream on a `PipeWire` connection, behind a safe
//! [`VideoStream`].
//!
//! The stream runs on a `pw_thread_loop`: libpipewire calls back on that
//! thread, where the newest shared-memory buffer of each pass is copied out
//! (older ones go straight back) and every other buffer is returned at
//! once. The reader waits on a condition variable for the next picture,
//! the stream reaching `streaming`, or its end ([`VideoStream::wait`]).
//! The copy keeps every pointer libpipewire hands out inside the callback
//! that receives it.

#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_int, c_void};
use std::os::fd::{IntoRawFd, OwnedFd};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::pod::{self, RawVideo};
use crate::sys::{self, Api, Opaque, PwBuffer, SpaDict, SpaDictItem, SpaHook};

/// One picture copied out of the stream.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Frame {
    /// The rows, `stride` bytes apart.
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    /// The `spa_video_format` (see [`crate::pod::video`]).
    pub format: u32,
}

/// How a stream ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamEnd {
    /// The shared node went away.
    Ended(String),
    /// The stream or its connection failed.
    Failed(String),
}

/// What happened since the last [`VideoStream::wait`].
#[derive(Debug, Default)]
pub struct Update {
    /// The stream reached `streaming`.
    pub streaming: bool,
    /// The newest picture, if one arrived.
    pub frame: Option<Frame>,
    /// The stream ended.
    pub end: Option<StreamEnd>,
}

#[derive(Default)]
struct State {
    format: Option<RawVideo>,
    frame: Option<Frame>,
    /// A picture's buffer handed back for reuse.
    spare: Vec<u8>,
    streaming: bool,
    end: Option<StreamEnd>,
}

/// What the callbacks share with the reader. Lives (boxed) at least as
/// long as the stream and core it is registered with.
struct Shared {
    api: &'static Api,
    state: Mutex<State>,
    wake: Condvar,
    stream: AtomicPtr<Opaque>,
    /// The `Buffers` parameter, 8-byte aligned.
    buffers: Vec<u64>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn end(&self, end: StreamEnd) {
        let mut state = self.state();
        state.end.get_or_insert(end);
        drop(state);
        self.wake.notify_all();
    }
}

/// A pod's bytes as 8-byte aligned words (pods are padded to 8 bytes).
fn aligned(pod: &[u8]) -> Vec<u64> {
    pod.chunks(8)
        .map(|chunk| {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            u64::from_ne_bytes(word)
        })
        .collect()
}

/// A C string's text, or `fallback` for a null pointer.
///
/// # Safety
///
/// `text` is null or a NUL-terminated string valid for the call.
unsafe fn text(text: *const c_char, fallback: &str) -> String {
    if text.is_null() {
        return fallback.to_owned();
    }
    // SAFETY: the caller passes a valid NUL-terminated string.
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

/// `state_changed`: errors and the stream's end, and `streaming`.
unsafe extern "C" fn on_state(data: *mut c_void, _old: c_int, new: c_int, error: *const c_char) {
    // SAFETY: `data` is the `Shared` registered with this listener, alive
    // until the stream is destroyed.
    let shared = unsafe { &*data.cast::<Shared>() };
    match new {
        sys::STATE_ERROR => {
            // SAFETY: libpipewire passes a valid error string or null.
            let message = unsafe { text(error, "unknown error") };
            shared.end(StreamEnd::Failed(format!(
                "the shared stream failed: {message}"
            )));
        }
        sys::STATE_STREAMING => {
            shared.state().streaming = true;
            shared.wake.notify_all();
        }
        // Only reached again after connecting: the shared node went away.
        sys::STATE_UNCONNECTED => shared.end(StreamEnd::Ended("the shared stream ended".into())),
        _ => {}
    }
}

/// `param_changed`: the negotiated format, answered with the buffers
/// wanted.
unsafe extern "C" fn on_param(data: *mut c_void, id: u32, param: *const c_void) {
    // SAFETY: as in `on_state`.
    let shared = unsafe { &*data.cast::<Shared>() };
    if id != pod::param::FORMAT {
        return;
    }
    let format = if param.is_null() {
        None
    } else {
        // SAFETY: a non-null param is a whole pod: its header's body size
        // bytes follow the 8-byte header.
        let size = unsafe { param.cast::<u32>().read_unaligned() };
        let Ok(size) = usize::try_from(size) else {
            return;
        };
        // SAFETY: as above, the pod spans its header and body.
        let bytes = unsafe { std::slice::from_raw_parts(param.cast::<u8>(), 8 + size) };
        pod::raw_video(bytes)
    };
    shared.state().format = format;
    if format.is_none() {
        return;
    }
    let stream = shared.stream.load(Ordering::Acquire);
    let mut params = [shared.buffers.as_ptr().cast::<c_void>()];
    // SAFETY: called on the loop thread with the stream alive; the pod
    // outlives the call, which copies it.
    unsafe { (shared.api.stream_update_params)(stream, params.as_mut_ptr(), 1) };
}

/// `process`: copies out the newest queued picture and returns every
/// buffer.
unsafe extern "C" fn on_process(data: *mut c_void) {
    // SAFETY: as in `on_state`.
    let shared = unsafe { &*data.cast::<Shared>() };
    let api = shared.api;
    let stream = shared.stream.load(Ordering::Acquire);
    let mut newest: *mut PwBuffer = std::ptr::null_mut();
    loop {
        // SAFETY: called on the loop thread with the stream alive.
        let buffer = unsafe { (api.stream_dequeue_buffer)(stream) };
        if buffer.is_null() {
            break;
        }
        if !newest.is_null() {
            // SAFETY: `newest` was dequeued from this stream and not
            // queued since.
            unsafe { (api.stream_queue_buffer)(stream, newest) };
        }
        newest = buffer;
    }
    if newest.is_null() {
        return;
    }
    // SAFETY: `newest` is a dequeued buffer, not yet given back.
    unsafe { copy_out(shared, newest) };
    // SAFETY: as above; it is given back exactly once.
    unsafe { (api.stream_queue_buffer)(stream, newest) };
}

/// Copies `buffer`'s first plane into a [`Frame`] when it holds a whole
/// shared-memory picture of the negotiated format.
///
/// # Safety
///
/// `buffer` is a buffer dequeued from the stream and not yet queued back.
unsafe fn copy_out(shared: &Shared, buffer: *mut PwBuffer) {
    let mut state = shared.state();
    let Some(format) = state.format else {
        return;
    };
    // SAFETY: a dequeued `pw_buffer` points at its `spa_buffer`.
    let spa = unsafe { (*buffer).buffer };
    // SAFETY: a non-null `spa_buffer` is valid while dequeued.
    if spa.is_null() || unsafe { (*spa).n_datas } == 0 {
        return;
    }
    // SAFETY: as above.
    let datas = unsafe { (*spa).datas };
    if datas.is_null() {
        return;
    }
    // SAFETY: `n_datas` is at least one, so `datas` holds one plane.
    let plane = unsafe { &*datas };
    if !matches!(plane.type_, pod::data::MEM_PTR | pod::data::MEM_FD)
        || plane.data.is_null()
        || plane.chunk.is_null()
    {
        return;
    }
    // SAFETY: a plane's chunk is valid while its buffer is dequeued.
    let chunk = unsafe { &*plane.chunk };
    if chunk.flags & sys::CHUNK_CORRUPTED != 0 {
        return;
    }
    let (Ok(offset), Ok(size), Ok(maxsize)) = (
        usize::try_from(chunk.offset),
        usize::try_from(chunk.size),
        usize::try_from(plane.maxsize),
    ) else {
        return;
    };
    // The chunk lies in the mapped plane, or nothing is read.
    let Some(end) = offset.checked_add(size).filter(|end| *end <= maxsize) else {
        return;
    };
    if size == 0 {
        return;
    }
    let stride = usize::try_from(chunk.stride)
        .ok()
        .filter(|stride| *stride > 0)
        .unwrap_or_else(|| usize::try_from(format.width).unwrap_or(0) * 4);
    // SAFETY: with `MAP_BUFFERS` the plane's `maxsize` bytes are mapped at
    // `data` while the buffer is dequeued, and `offset..end` lies in them.
    let bytes =
        unsafe { std::slice::from_raw_parts(plane.data.cast::<u8>().cast_const(), maxsize) };
    let mut data = std::mem::take(&mut state.spare);
    data.clear();
    data.extend_from_slice(&bytes[offset..end]);
    let old = state.frame.replace(Frame {
        data,
        width: format.width,
        height: format.height,
        stride,
        format: format.format,
    });
    if let Some(old) = old {
        state.spare = old.data;
    }
    drop(state);
    shared.wake.notify_all();
}

/// `error` on the core: the connection failed.
unsafe extern "C" fn on_core_error(
    data: *mut c_void,
    _seq: c_int,
    _res: c_int,
    message: *const c_char,
) {
    // SAFETY: as in `on_state`.
    let shared = unsafe { &*data.cast::<Shared>() };
    // SAFETY: libpipewire passes a valid message or null.
    let message = unsafe { text(message, "unknown error") };
    shared.end(StreamEnd::Failed(format!(
        "the PipeWire connection failed: {message}"
    )));
}

static STREAM_EVENTS: sys::StreamEvents = sys::StreamEvents {
    version: sys::STREAM_EVENTS_VERSION,
    destroy: None,
    state_changed: Some(on_state),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_param),
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_process),
    drained: None,
    command: None,
    trigger_done: None,
};

static CORE_EVENTS: sys::ProxyEvents = sys::ProxyEvents {
    version: sys::PROXY_EVENTS_VERSION,
    destroy: None,
    bound: None,
    removed: None,
    done: None,
    error: Some(on_core_error),
    bound_props: None,
};

/// A raw video input stream linked to one node of a `PipeWire` remote.
/// Dropping it disconnects and frees everything.
pub struct VideoStream {
    api: &'static Api,
    thread_loop: *mut Opaque,
    context: *mut Opaque,
    core: *mut Opaque,
    stream: *mut Opaque,
    shared: Box<Shared>,
    core_hook: Box<SpaHook>,
    stream_hook: Box<SpaHook>,
}

impl VideoStream {
    /// Connects to the `PipeWire` remote `remote` (a connection the
    /// desktop portal opened) and links a stream named `name` to `node`,
    /// offering `formats` (`spa_video_format`s, the first preferred) as
    /// shared-memory raw video of any size and rate.
    ///
    /// # Errors
    ///
    /// libpipewire cannot be loaded ([`crate::available`]), or a step of
    /// the connection fails; the text says which.
    pub fn connect(
        remote: OwnedFd,
        node: u32,
        name: &CStr,
        formats: &[u32],
    ) -> Result<Self, String> {
        let api = sys::api().map_err(|error| error.to_string())?;
        let offer = aligned(&pod::video_offer(formats));
        let mut this = Self {
            api,
            thread_loop: std::ptr::null_mut(),
            context: std::ptr::null_mut(),
            core: std::ptr::null_mut(),
            stream: std::ptr::null_mut(),
            shared: Box::new(Shared {
                api,
                state: Mutex::new(State::default()),
                wake: Condvar::new(),
                stream: AtomicPtr::new(std::ptr::null_mut()),
                buffers: aligned(&pod::shared_memory_buffers()),
            }),
            core_hook: Box::new(SpaHook::zeroed()),
            stream_hook: Box::new(SpaHook::zeroed()),
        };
        // SAFETY: the name is a valid C string; no properties.
        this.thread_loop = unsafe { (api.thread_loop_new)(name.as_ptr(), std::ptr::null()) };
        if this.thread_loop.is_null() {
            return Err("no PipeWire thread loop can be created".into());
        }
        // SAFETY: the thread loop is valid.
        let loop_ = unsafe { (api.thread_loop_get_loop)(this.thread_loop) };
        // SAFETY: the loop is valid; no properties or user data.
        this.context = unsafe { (api.context_new)(loop_, std::ptr::null_mut(), 0) };
        if this.context.is_null() {
            return Err("no PipeWire context can be created".into());
        }
        // SAFETY: the thread loop is valid and not yet running.
        if unsafe { (api.thread_loop_start)(this.thread_loop) } < 0 {
            return Err("the PipeWire thread loop cannot start".into());
        }
        // SAFETY: the thread loop runs; its lock guards every call below.
        unsafe { (api.thread_loop_lock)(this.thread_loop) };
        let linked = this.link(remote, node, name, &offer);
        // SAFETY: locked above.
        unsafe { (api.thread_loop_unlock)(this.thread_loop) };
        linked.map(|()| this)
    }

    /// Connects the core and the stream, with the thread loop locked.
    fn link(
        &mut self,
        remote: OwnedFd,
        node: u32,
        name: &CStr,
        offer: &[u64],
    ) -> Result<(), String> {
        let api = self.api;
        let data = std::ptr::from_ref::<Shared>(&self.shared)
            .cast_mut()
            .cast::<c_void>();
        // SAFETY: the context is valid; the descriptor's ownership passes
        // to the core.
        self.core = unsafe {
            (api.context_connect_fd)(self.context, remote.into_raw_fd(), std::ptr::null_mut(), 0)
        };
        if self.core.is_null() {
            return Err("the desktop's PipeWire connection cannot be used".into());
        }
        // SAFETY: a core is a proxy; the hook and the shared state are
        // boxed and outlive the core (see `Drop`).
        unsafe {
            (api.proxy_add_listener)(
                self.core,
                &raw mut *self.core_hook,
                &raw const CORE_EVENTS,
                data,
            );
        }
        let items = [
            SpaDictItem {
                key: c"media.type".as_ptr(),
                value: c"Video".as_ptr(),
            },
            SpaDictItem {
                key: c"media.category".as_ptr(),
                value: c"Capture".as_ptr(),
            },
            SpaDictItem {
                key: c"media.role".as_ptr(),
                value: c"Screen".as_ptr(),
            },
        ];
        let dict = SpaDict {
            flags: 0,
            n_items: 3,
            items: items.as_ptr(),
        };
        // SAFETY: the dictionary's strings outlive the call, which copies
        // them.
        let properties = unsafe { (api.properties_new_dict)(&raw const dict) };
        // SAFETY: the core is valid; the stream takes the properties.
        self.stream = unsafe { (api.stream_new)(self.core, name.as_ptr(), properties) };
        if self.stream.is_null() {
            return Err("no PipeWire stream can be created".into());
        }
        self.shared.stream.store(self.stream, Ordering::Release);
        // SAFETY: as for the core's listener.
        unsafe {
            (api.stream_add_listener)(
                self.stream,
                &raw mut *self.stream_hook,
                &raw const STREAM_EVENTS,
                data,
            );
        }
        let mut params = [offer.as_ptr().cast::<c_void>()];
        // SAFETY: the stream is valid; the offer outlives the call, which
        // copies it.
        let connected = unsafe {
            (api.stream_connect)(
                self.stream,
                sys::DIRECTION_INPUT,
                node,
                sys::FLAGS_AUTOCONNECT_MAP_BUFFERS,
                params.as_mut_ptr(),
                1,
            )
        };
        if connected < 0 {
            return Err(format!(
                "the shared stream cannot be connected (error {connected})"
            ));
        }
        Ok(())
    }

    /// Waits up to `timeout` for a picture, `streaming`, or the end, and
    /// takes what happened.
    #[must_use]
    pub fn wait(&self, timeout: Duration) -> Update {
        let state = self.shared.state();
        let (mut state, _) = self
            .shared
            .wake
            .wait_timeout_while(state, timeout, |state| {
                !state.streaming && state.frame.is_none() && state.end.is_none()
            })
            .unwrap_or_else(PoisonError::into_inner);
        Update {
            streaming: std::mem::take(&mut state.streaming),
            frame: state.frame.take(),
            end: state.end.take(),
        }
    }

    /// Hands a read picture's buffer back for the next copy.
    pub fn recycle(&self, frame: Frame) {
        let mut state = self.shared.state();
        if state.spare.capacity() < frame.data.capacity() {
            state.spare = frame.data;
        }
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        let api = self.api;
        if self.thread_loop.is_null() {
            return;
        }
        // SAFETY: the thread loop is valid; once stopped nothing runs on
        // it, so the objects below are destroyed without its lock, stream
        // first, then core, context, and loop (their creation's reverse).
        // The hooks and shared state are freed only after.
        unsafe { (api.thread_loop_stop)(self.thread_loop) };
        if !self.stream.is_null() {
            // SAFETY: as above.
            unsafe { (api.stream_disconnect)(self.stream) };
            // SAFETY: as above.
            unsafe { (api.stream_destroy)(self.stream) };
        }
        if !self.core.is_null() {
            // SAFETY: as above.
            unsafe { (api.core_disconnect)(self.core) };
        }
        if !self.context.is_null() {
            // SAFETY: as above.
            unsafe { (api.context_destroy)(self.context) };
        }
        // SAFETY: as above.
        unsafe { (api.thread_loop_destroy)(self.thread_loop) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pods_are_aligned_whole() {
        let offer = pod::video_offer(&[pod::video::BGRX]);
        let words = aligned(&offer);
        assert_eq!(words.len() * 8, offer.len());
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_ne_bytes()).collect();
        assert_eq!(bytes, offer);
    }
}
