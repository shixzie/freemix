//! Native slideshow sources (IN-009).
//!
//! A slideshow input owns one live source in the native source registry. Each
//! slide is decoded off the loop (fitted to the show raster with the
//! slideshow's fit, EXIF-oriented and converted to sRGB) and uploaded to the
//! GPU exactly once; every frame whose slides, Ken Burns crop, or fade mix
//! changed composes the incoming slide over the outgoing one on the GPU into
//! the source's picture, before the frame renders. Nothing is uploaded per
//! frame. The next listed slide is decoded ahead so an ordered show's next
//! slide is ready when it is due; a slide still decoding when it is due
//! leaves the previous picture up until it arrives.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;

use fm_color::NativeWorkingFrame;
use fm_engine::{EngineMediaFault, EngineSlideCrop, EngineSlideshowFrame};
use fm_frame::CpuVideoFrame;
use fm_types::{InputId, OutputScaling, VideoDimensions};
use freemixd::native_media::{
    NativeMediaRuntime, NativeSlideLayer, NativeSourcePlayback, NativeSourceRenderError,
};

use crate::stills::{decode_picture_file, resolve_picture_path};

/// Most uploaded slides kept beyond the ones on air and decoded ahead.
const MAX_SPARE_SLIDES: usize = 4;
/// Most slide decodes in flight at once.
const MAX_DECODES: usize = 4;

/// One slide picture: its file and the fit it was placed with.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SlideKey {
    asset_uri: Arc<str>,
    fit: OutputScaling,
}

struct Uploaded {
    key: SlideKey,
    frame: NativeWorkingFrame,
    /// The fitted CPU picture, which thumbnails show.
    picture: Arc<CpuVideoFrame>,
}

struct Decoding {
    key: SlideKey,
    result: mpsc::Receiver<Result<CpuVideoFrame, EngineMediaFault>>,
}

/// What a slideshow source shows now.
#[derive(Clone, Debug, PartialEq)]
struct Presented {
    incoming: Option<(SlideKey, EngineSlideCrop)>,
    outgoing: Option<(SlideKey, EngineSlideCrop)>,
    mix: u16,
}

/// Every slideshow source's uploaded slides; see the module documentation.
pub(crate) struct NativeSlides {
    assets_root: PathBuf,
    dimensions: VideoDimensions,
    /// Most recently used last.
    uploaded: Vec<Uploaded>,
    decoding: Vec<Decoding>,
    /// Slides that did not decode, not tried again until the listing
    /// changes.
    failed: BTreeSet<SlideKey>,
    presented: Vec<(InputId, Presented)>,
}

impl NativeSlides {
    pub(crate) fn new(assets_root: PathBuf, dimensions: VideoDimensions) -> Self {
        Self {
            assets_root,
            dimensions,
            uploaded: Vec::new(),
            decoding: Vec::new(),
            failed: BTreeSet::new(),
            presented: Vec::new(),
        }
    }

    /// Forgets `input`'s picture (its source was removed or replaced).
    pub(crate) fn forget(&mut self, input: InputId) {
        self.presented.retain(|(presented, _)| *presented != input);
    }

    /// Uploads every slide decoded since the last call, then composes each
    /// slideshow of `frames` whose picture changed into its source. `fits`
    /// gives each slideshow's fit and `ahead` the slides to decode ahead.
    ///
    /// # Errors
    ///
    /// Returns a GPU composition failure.
    pub(crate) fn present(
        &mut self,
        runtime: &NativeMediaRuntime,
        playback: &mut NativeSourcePlayback,
        frames: &[(EngineSlideshowFrame, OutputScaling)],
        ahead: &[(Arc<str>, OutputScaling)],
    ) -> Result<(), NativeSourceRenderError> {
        self.receive(runtime);
        let mut wanted = Vec::new();
        for (frame, fit) in frames {
            let key = |view: &fm_engine::EngineSlideView| {
                (
                    SlideKey {
                        asset_uri: Arc::clone(&view.slide.asset_uri),
                        fit: *fit,
                    },
                    view.crop,
                )
            };
            let next = Presented {
                incoming: frame.incoming.as_ref().map(key),
                outgoing: frame.outgoing.as_ref().map(key),
                mix: frame.mix_basis_points,
            };
            wanted.extend(next.incoming.iter().map(|(key, _)| key.clone()));
            wanted.extend(next.outgoing.iter().map(|(key, _)| key.clone()));
            self.present_one(runtime, playback, frame.input, next)?;
        }
        if frames.iter().any(|(frame, _)| frame.changed) {
            // A new listing may have repaired a file.
            self.failed.clear();
        }
        for (asset_uri, fit) in ahead {
            let key = SlideKey {
                asset_uri: Arc::clone(asset_uri),
                fit: *fit,
            };
            self.start(&key);
            wanted.push(key);
        }
        self.evict(&wanted);
        Ok(())
    }

    fn present_one(
        &mut self,
        runtime: &NativeMediaRuntime,
        playback: &mut NativeSourcePlayback,
        input: InputId,
        next: Presented,
    ) -> Result<(), NativeSourceRenderError> {
        let current = self
            .presented
            .iter()
            .find(|(presented, _)| *presented == input)
            .map(|(_, presented)| presented.clone());
        if current.as_ref() == Some(&next) {
            return Ok(());
        }
        for (key, _) in next.incoming.iter().chain(&next.outgoing) {
            self.start(key);
        }
        let ready = |key: &SlideKey| self.uploaded.iter().position(|slide| slide.key == *key);
        // The incoming slide must be on the GPU (or have failed, showing
        // nothing) before the picture changes.
        let incoming = match &next.incoming {
            Some((key, crop)) => match ready(key) {
                Some(index) => Some((index, *crop)),
                None if self.failed.contains(key) => None,
                None => return Ok(()),
            },
            None => None,
        };
        let outgoing = next
            .outgoing
            .as_ref()
            .and_then(|(key, crop)| Some((ready(key)?, *crop)));
        let changed_slide = current.is_none_or(|current| {
            current.incoming.as_ref().map(|(key, _)| key)
                != next.incoming.as_ref().map(|(key, _)| key)
        });
        let mut layers = Vec::with_capacity(2);
        if let Some((index, crop)) = outgoing {
            layers.push(NativeSlideLayer {
                frame: &self.uploaded[index].frame,
                crop: crop.fractions(),
                opacity: u8::MAX,
            });
        }
        if let Some((index, crop)) = incoming {
            let opacity = if outgoing.is_some() {
                opacity_of(next.mix)
            } else {
                u8::MAX
            };
            layers.push(NativeSlideLayer {
                frame: &self.uploaded[index].frame,
                crop: crop.fractions(),
                opacity,
            });
        }
        let picture = incoming
            .filter(|_| changed_slide)
            .map(|(index, _)| Arc::clone(&self.uploaded[index].picture));
        runtime.present_slides_blocking(playback, input, &layers, picture)?;
        self.presented.retain(|(presented, _)| *presented != input);
        self.presented.push((input, next));
        Ok(())
    }

    /// Starts decoding `key` unless it is uploaded, decoding, or failed.
    fn start(&mut self, key: &SlideKey) {
        if self.uploaded.iter().any(|slide| slide.key == *key)
            || self.decoding.iter().any(|decoding| decoding.key == *key)
            || self.failed.contains(key)
            || self.decoding.len() >= MAX_DECODES
        {
            return;
        }
        let (sender, result) = mpsc::sync_channel(1);
        let (dimensions, fit) = (self.dimensions, key.fit);
        let (assets_root, asset_uri) = (self.assets_root.clone(), Arc::clone(&key.asset_uri));
        let spawned = thread::Builder::new()
            .name("freemix-slide".into())
            .spawn(move || {
                // Resolved like every other bundle asset: a link out of the
                // assets never decodes.
                let decoded = resolve_picture_path(&assets_root, &asset_uri).and_then(|path| {
                    decode_picture_file(&path, dimensions, fit, crate::native_clock_domain())
                });
                let _ = sender.send(decoded);
            });
        if spawned.is_ok() {
            self.decoding.push(Decoding {
                key: key.clone(),
                result,
            });
        } else {
            self.failed.insert(key.clone());
        }
    }

    /// Uploads every finished decode once.
    fn receive(&mut self, runtime: &NativeMediaRuntime) {
        let mut index = 0;
        while index < self.decoding.len() {
            let outcome = match self.decoding[index].result.try_recv() {
                Ok(outcome) => outcome,
                Err(mpsc::TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => Err(EngineMediaFault::DecodeFailed),
            };
            let key = self.decoding.swap_remove(index).key;
            let uploaded = outcome.ok().and_then(|picture| {
                let frame = runtime.upload_still_blocking(&picture).ok()?;
                Some((frame, picture))
            });
            if let Some((frame, picture)) = uploaded {
                self.uploaded.push(Uploaded {
                    key,
                    frame,
                    picture: Arc::new(picture),
                });
            } else {
                eprintln!(
                    "FREEMIXD_SLIDE\tv=1\tfailed={}",
                    key.asset_uri.escape_default()
                );
                self.failed.insert(key);
            }
        }
    }

    /// Keeps every slide on air or wanted ahead, and at most
    /// [`MAX_SPARE_SLIDES`] others (the most recently used).
    fn evict(&mut self, wanted: &[SlideKey]) {
        let on_air = self
            .presented
            .iter()
            .flat_map(|(_, presented)| presented.incoming.iter().chain(&presented.outgoing))
            .map(|(key, _)| key)
            .collect::<Vec<_>>();
        let keep = |key: &SlideKey| on_air.contains(&key) || wanted.contains(key);
        let mut spare = self
            .uploaded
            .iter()
            .filter(|slide| !keep(&slide.key))
            .count();
        let mut index = 0;
        while spare > MAX_SPARE_SLIDES && index < self.uploaded.len() {
            if keep(&self.uploaded[index].key) {
                index += 1;
            } else {
                self.uploaded.remove(index);
                spare -= 1;
            }
        }
    }
    /// Bytes of uploaded slide textures, for the daemon's accounting.
    pub(crate) fn retained_bytes(&self) -> u64 {
        self.uploaded.iter().fold(0_u64, |total, slide| {
            let texture = slide.frame.texture();
            let bytes = u64::from(texture.width())
                .saturating_mul(u64::from(texture.height()))
                .saturating_mul(8);
            total.saturating_add(bytes)
        })
    }
}

/// The fade opacity of `mix` basis points.
fn opacity_of(mix: u16) -> u8 {
    let scaled = u32::from(mix.min(10_000)) * u32::from(u8::MAX) / 10_000;
    u8::try_from(scaled).unwrap_or(u8::MAX)
}
