//! Texture readback reuses its staging buffers.
//!
//! A steady stream of same-sized readbacks (a Program frame read back every
//! frame) must not create a staging buffer per readback, and a reused buffer
//! must never hand back the previous readback's bytes. Runs on any wgpu
//! adapter (including lavapipe); without one the test skips, unless
//! `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

#![cfg(feature = "native-wgpu")]

use std::{
    future::Future,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
    time::{Duration, Instant},
};

use fm_gpu::{
    NativeBackend, NativeContext, NativePendingReadback, NativeReadbackPoll, NativeTextureReadback,
};

struct ThreadWaker(thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}

/// Any native adapter, or `None` (skip) when none is available.
fn native_context(test: &str) -> Option<NativeContext> {
    match block_on(NativeContext::new([
        NativeBackend::Vulkan,
        NativeBackend::Metal,
        NativeBackend::Dx12,
        NativeBackend::Gl,
    ])) {
        Ok(context) => Some(context),
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("{test} skipped: no native adapter ({error})");
            None
        }
    }
}

/// RGBA8 pixels of a `width`x`height` image unique to `seed`.
fn pattern(width: u32, height: u32, seed: u8) -> Vec<u8> {
    (0..width * height * 4)
        .map(|index| {
            u8::try_from(index % 251)
                .unwrap()
                .wrapping_mul(7)
                .wrapping_add(seed)
        })
        .collect()
}

#[test]
fn same_sized_readbacks_reuse_one_staging_buffer_and_return_their_own_bytes() {
    let Some(context) = native_context("readback staging reuse") else {
        return;
    };
    // 70 pixels is not a multiple of 64, so rows are padded in the staging
    // buffer and unpacked on the way out.
    let (width, height) = (70, 9);
    let mut created_after_first = None;
    for frame in 0..12_u8 {
        let pixels = pattern(width, height, frame);
        let texture =
            block_on(context.upload_rgba8(width, height, width as usize * 4, &pixels)).unwrap();
        let readback = block_on(context.readback_rgba8(&texture)).unwrap();
        assert_eq!((readback.width, readback.height), (width, height));
        assert_eq!(readback.stride, width * 4);
        assert_eq!(readback.rgba, pixels, "frame {frame} read back other bytes");
        let created = context.readback_staging_buffers_created();
        match created_after_first {
            None => {
                assert_eq!(created, 1, "the first readback creates its buffer");
                created_after_first = Some(created);
            }
            Some(first) => assert_eq!(
                created, first,
                "frame {frame}: a steady-state readback created a staging buffer"
            ),
        }
    }

    // Another size gets its own buffer, and both sizes keep reusing theirs
    // when they alternate.
    let small = pattern(3, 2, 99);
    let small_texture = block_on(context.upload_rgba8(3, 2, 12, &small)).unwrap();
    let large = pattern(width, height, 42);
    let large_texture =
        block_on(context.upload_rgba8(width, height, width as usize * 4, &large)).unwrap();
    for _ in 0..4 {
        assert_eq!(
            block_on(context.readback_rgba8(&small_texture))
                .unwrap()
                .rgba,
            small
        );
        assert_eq!(
            block_on(context.readback_rgba8(&large_texture))
                .unwrap()
                .rgba,
            large
        );
    }
    assert_eq!(context.readback_staging_buffers_created(), 2);
}

/// Polls `pending` until it finishes, as a frame loop would between frames.
fn finish(context: &NativeContext, mut pending: NativePendingReadback) -> NativeTextureReadback {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match context.poll_readback(pending).unwrap() {
            NativeReadbackPoll::Ready(readback) => return readback,
            NativeReadbackPoll::Pending(again) => {
                assert!(Instant::now() < deadline, "readback never finished");
                pending = again;
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

#[test]
fn nonblocking_readbacks_return_their_own_bytes_and_reuse_staging_buffers() {
    let Some(context) = native_context("nonblocking readback") else {
        return;
    };
    let (width, height) = (70, 9);
    let first = pattern(width, height, 1);
    let texture =
        block_on(context.upload_rgba8(width, height, width as usize * 4, &first)).unwrap();
    let pending = block_on(context.begin_readback(&texture)).unwrap();
    assert_eq!((pending.width(), pending.height()), (width, height));
    // Work submitted after the copy does not change what it returns.
    let second = pattern(width, height, 2);
    let rewritten =
        block_on(context.upload_rgba8(width, height, width as usize * 4, &second)).unwrap();
    let readback = finish(&context, pending);
    assert_eq!(readback.stride, width * 4);
    assert_eq!(readback.bytes, first);
    let created = context.readback_staging_buffers_created();
    for _ in 0..4 {
        let pending = block_on(context.begin_readback(&rewritten)).unwrap();
        assert_eq!(finish(&context, pending).bytes, second);
    }
    assert_eq!(
        context.readback_staging_buffers_created(),
        created,
        "steady nonblocking readbacks reuse their staging buffer"
    );
    // A readback dropped unfinished is harmless.
    drop(block_on(context.begin_readback(&rewritten)).unwrap());
    assert_eq!(
        block_on(context.readback_rgba8(&rewritten)).unwrap().rgba,
        second
    );
}
