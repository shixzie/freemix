//! Local title animation commands: apply a preset, stretch a phase, and
//! render one title input at one frame of a phase.
//!
//! Edits rewrite the stored `.fmtitle` template in place (every title input
//! naming it sees the change) and refuse a bundle a running daemon holds,
//! like every other local mutation. An edit whose clip would exceed the
//! protocol's `MAX_TITLE_ANIMATION_BYTES` wire bound (the bound `fm-control`
//! holds every daemon edit to) is refused and nothing is saved.
//!
//! Rendering is read-only and shares `freemixd`'s title renderer
//! (`fm-title-render`): it draws the input's own field values at the
//! template's canvas size with the bundle's fonts, decoded PNG and JPEG
//! images, and production clocks read at a deterministic time (`--at` for
//! wall-clock UTC, the clip position for time since the take). Images that
//! do not load keep the placeholder and are reported.

use std::{fs, path::Path};

use fm_clock::production::UtcInstant;
use fm_persistence::{ProjectStore, StoredTitleTemplate};
use fm_title_render::{TitleAssets, TitleTime, render_title_output};
use fm_titles::{
    AnimSample, ElementId, FontFace, MAX_FONT_BYTES,
    fm_animation::{AnimationPreset, ClipPosition, Phase},
};
use fm_types::InputId;

use crate::{
    app::{AppFailure, AppResult, inspect_stored_project, load_stored_project, write_title_frame},
    args::TitlePhase,
};

const fn phase(phase: TitlePhase) -> Phase {
    match phase {
        TitlePhase::In => Phase::In,
        TitlePhase::Out => Phase::Out,
    }
}

const fn phase_name(phase: TitlePhase) -> &'static str {
    match phase {
        TitlePhase::In => "in",
        TitlePhase::Out => "out",
    }
}

/// Loads the template at `uri` after refusing a bundle a daemon holds.
fn editable_template(path: &Path, uri: &str) -> AppResult<(ProjectStore, StoredTitleTemplate)> {
    load_stored_project(path)?;
    let store = ProjectStore::new(path)?;
    let stored = store
        .load_title_template(uri)
        .map_err(|error| AppFailure(format!("title template {uri}: {error}")))?;
    Ok((store, stored))
}

fn save(
    command: &str,
    store: &ProjectStore,
    uri: &str,
    stored: &StoredTitleTemplate,
) -> AppResult<()> {
    // The same bound the daemon holds every edit to: a clip without a wire
    // encoding could never be carried by an event or snapshot.
    if !fm_control::title_clip_fits_wire(&stored.template.animation.clip) {
        return Err(AppFailure(format!(
            "title template {uri}: the edited animation exceeds the {} byte protocol limit \
             (MAX_TITLE_ANIMATION_BYTES) and was not saved; use fewer keys or tracks",
            fm_protocol::MAX_TITLE_ANIMATION_BYTES
        ))
        .into());
    }
    store
        .replace_title_template(uri, stored)
        .map_err(|error| AppFailure(format!("title template {uri}: {error}")))?;
    let clip = &stored.template.animation.clip;
    println!(
        "{command}: {uri} revision={} in_frames={} out_frames={} tracks={}",
        stored.template.animation.revision,
        clip.in_frames(),
        clip.out_frames(),
        clip.tracks().len()
    );
    Ok(())
}

pub fn preset(
    path: &Path,
    uri: &str,
    element: u128,
    title_phase: TitlePhase,
    preset: &str,
    frames: u32,
) -> AppResult<()> {
    let element = ElementId::new(
        core::num::NonZeroU128::new(element)
            .ok_or_else(|| AppFailure("element id must be nonzero".into()))?,
    );
    // `user:<id>` names one of the project's own presets (GX-011).
    if let Some(motion) = crate::presets::user_motion(path, preset)? {
        let (store, mut stored) = editable_template(path, uri)?;
        stored
            .template
            .apply_animation_motion(element, phase(title_phase), &motion, frames)
            .map_err(|error| AppFailure(error.to_string()))?;
        return save("title-animation-preset", &store, uri, &stored);
    }
    let Some(preset) = AnimationPreset::from_id(preset) else {
        let known = AnimationPreset::ALL.map(AnimationPreset::id).join(", ");
        return Err(AppFailure(format!(
            "unknown animation preset `{preset}` (one of {known}, or user:<id>)"
        ))
        .into());
    };
    let (store, mut stored) = editable_template(path, uri)?;
    stored
        .template
        .apply_animation_preset(element, phase(title_phase), preset, frames)
        .map_err(|error| AppFailure(error.to_string()))?;
    save("title-animation-preset", &store, uri, &stored)
}

/// `preset-save`: adds `element`'s keys in one phase of the template at
/// `uri` to the project's preset library as `name`.
pub fn save_preset(
    path: &Path,
    uri: &str,
    element: u128,
    title_phase: TitlePhase,
    name: &str,
) -> AppResult<()> {
    let element = ElementId::new(
        core::num::NonZeroU128::new(element)
            .ok_or_else(|| AppFailure("element id must be nonzero".into()))?,
    );
    let (_, stored) = editable_template(path, uri)?;
    let motion = stored
        .template
        .capture_animation_motion(element, phase(title_phase))
        .map_err(|error| AppFailure(format!("element {element}: {error}")))?;
    crate::presets::add_animation(path, name, phase(title_phase), &motion, "preset-save")
}

pub fn stretch(path: &Path, uri: &str, title_phase: TitlePhase, frames: u32) -> AppResult<()> {
    let (store, mut stored) = editable_template(path, uri)?;
    stored
        .template
        .stretch_animation(phase(title_phase), frames)
        .map_err(|error| AppFailure(error.to_string()))?;
    save("title-animation-stretch", &store, uri, &stored)
}

/// Parses exactly `YYYY-MM-DDTHH:MM:SSZ`, the form `.fmtitle` clock tables
/// use for UTC instants.
fn parse_utc(text: &str) -> AppResult<UtcInstant> {
    let invalid = || {
        AppFailure(format!(
            "--at `{text}` is not a YYYY-MM-DDTHH:MM:SSZ UTC time"
        ))
    };
    let bytes = text.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return Err(invalid().into());
    }
    let number = |range: core::ops::Range<usize>| -> Result<u32, AppFailure> {
        let digits = &text[range];
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        digits.parse().map_err(|_| invalid())
    };
    let narrow = |value: u32| u8::try_from(value).map_err(|_| invalid());
    Ok(UtcInstant::from_utc_civil(
        i32::try_from(number(0..4)?).map_err(|_| invalid())?,
        narrow(number(5..7)?)?,
        narrow(number(8..10)?)?,
        narrow(number(11..13)?)?,
        narrow(number(14..16)?)?,
        narrow(number(17..19)?)?,
        0,
    )
    .map_err(|_| invalid())?)
}

/// Renders title input `input` at `frame` of `title_phase` to a PPM over
/// black, through the renderer `freemixd` draws titles with
/// (`fm-title-render`): the bundle's fonts, decoded PNG and JPEG images,
/// and production clocks.
///
/// The title is taken on air at the first In frame, and Out follows the
/// whole In phase with no hold: the rendered frame lies `frame` (In) or
/// `in_frames + frame` (Out) clip frames after the take, on the clip's
/// timebase. That is the time since-take clocks and scene clocks read;
/// time-of-day and countdown clocks read `at` (default the Unix epoch), so
/// every render is deterministic.
pub fn render_frame(
    path: &Path,
    input: u128,
    title_phase: TitlePhase,
    frame: u32,
    at: Option<&str>,
    output: &Path,
) -> AppResult<()> {
    let id = InputId::new(
        core::num::NonZeroU128::new(input)
            .ok_or_else(|| AppFailure("input id must be nonzero".into()))?,
    );
    let utc = at.map_or(Ok(UtcInstant::UNIX_EPOCH), parse_utc)?;
    let project = inspect_stored_project(path)?;
    let title = fm_title_render::title_of_input(project.project(), id)
        .ok_or_else(|| AppFailure(format!("input {input} is not a title input")))?
        .clone();
    let store = ProjectStore::new(path)?;
    // The shared loader leaves out what does not load; name the failure.
    let stored = store
        .load_title_template(&title.template_uri)
        .map_err(|error| AppFailure(format!("title template {}: {error}", title.template_uri)))?;
    for font in &stored.fonts {
        let file = store
            .resolve_asset_uri(&font.asset_uri)
            .map_err(|error| AppFailure(format!("font {}: {error}", font.asset_uri)))?;
        let size = fs::metadata(&file)?.len();
        if size > u64::try_from(MAX_FONT_BYTES).unwrap_or(u64::MAX) {
            return Err(AppFailure(format!("font {} is too large", font.asset_uri)).into());
        }
        FontFace::from_bytes(fs::read(&file)?)
            .map_err(|error| AppFailure(format!("font {}: {error}", font.asset_uri)))?;
    }
    let clip = &stored.template.animation.clip;
    let position = ClipPosition::frame(frame);
    let (sample, since_take) = match title_phase {
        TitlePhase::In => (AnimSample::In(position), u64::from(frame)),
        TitlePhase::Out => (
            AnimSample::Out(position),
            u64::from(clip.in_frames()) + u64::from(frame),
        ),
    };
    let assets = TitleAssets::load(&store, project.project());
    let time = TitleTime {
        frame: since_take,
        rate: clip.timebase().rate(),
        utc,
        taken_frame: Some(0),
        pose: sample,
    };
    let rendered = render_title_output(&assets, id, &title, time)
        .map_err(|error| AppFailure(error.to_string()))?;
    write_title_frame(output, &rendered.frame)?;
    println!(
        "title-render-frame: input {input} phase={} frame={frame} {}x{} to {}",
        phase_name(title_phase),
        rendered.frame.width(),
        rendered.frame.height(),
        output.display()
    );
    for degraded in &rendered.report.degraded {
        println!("degraded: {degraded}");
    }
    // An image named by the template or the input but not decoded (absent,
    // unreadable, or not PNG or JPEG within the bounds) draws the
    // placeholder.
    let mut placeholders = assets
        .placeholder_images(&title)
        .into_iter()
        .map(|(field, asset)| format!("field {field} {asset}"))
        .collect::<Vec<_>>();
    for missing in &rendered.report.missing_images {
        let line = format!("field {} {}", missing.field, missing.asset);
        if !placeholders.contains(&line) {
            placeholders.push(line);
        }
    }
    for placeholder in placeholders {
        println!("placeholder image: {placeholder}");
    }
    Ok(())
}
