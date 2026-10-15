//! Fullscreen display outputs (OR-011): daemon status records, startup
//! validation, the diagnostic inventory-file display provider, and the
//! headless presenter that exercises the same scaled placement off-screen.
//!
//! Real borderless windows are driven by `program_surface` (feature
//! `display-surface`); both paths share the portable
//! [`fm_io_api::display::DisplayOutputTracker`] hot-plug state machine and
//! emit one `FREEMIXD_DISPLAY_OUTPUT` record per state change.

use std::{
    collections::BTreeSet,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use fm_io_api::display::{
    DisplayEnumerationError, DisplayId, DisplayInfo, DisplayMode, DisplayOutputConfig,
    DisplayOutputSet, DisplayOutputSource, DisplayOutputState, DisplayProvider, HdrCapability,
};

pub(crate) const DISPLAY_OUTPUT_PREFIX: &str = "FREEMIXD_DISPLAY_OUTPUT";
#[cfg(feature = "native-media")]
pub(crate) const DISPLAY_FRAME_PREFIX: &str = "FREEMIXD_DISPLAY_FRAME";

/// Replaces tabs, newlines, and other control characters so platform text
/// cannot break a tab-separated record.
fn record_text(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect()
}

/// One `FREEMIXD_DISPLAY_OUTPUT` record for output `index` (zero-based).
pub(crate) fn status_record(
    index: usize,
    config: &DisplayOutputConfig,
    state: &DisplayOutputState,
) -> String {
    let head = format!("{DISPLAY_OUTPUT_PREFIX}\tv=1\toutput={}", index + 1);
    let tail = format!(
        "source={}\tscaling={}\tborderless=true\tcursor=hidden",
        config.source, config.scaling
    );
    match state {
        DisplayOutputState::Waiting(reason) => {
            format!("{head}\tstate=waiting\treason={}\t{tail}", reason.as_str())
        }
        DisplayOutputState::Active(active) => format!(
            "{head}\tstate=active\tdisplay={}\tname={}\tmode={}\tmode_fallback={}\thdr={}\t{tail}",
            record_text(active.id.as_str()),
            record_text(&active.name),
            active.mode,
            active.mode_fallback,
            active.hdr.as_str(),
        ),
    }
}

/// Validates each output's source against what the native daemon renders:
/// Program and declared project outputs. Preview is not composited by the
/// native daemon.
pub(crate) fn validate_sources(
    set: &DisplayOutputSet,
    project_outputs: &[fm_types::OutputId],
) -> Result<(), String> {
    for (index, config) in set.outputs().iter().enumerate() {
        match config.source {
            DisplayOutputSource::Program => {}
            DisplayOutputSource::Preview => {
                return Err(format!(
                    "display output {} selects source=preview, which the native daemon does not \
                     composite; select program or output:<id>",
                    index + 1
                ));
            }
            DisplayOutputSource::Output(output) => {
                if !project_outputs.contains(&output) {
                    return Err(format!(
                        "display output {} selects source=output:{output}, which the project does \
                         not declare",
                        index + 1
                    ));
                }
            }
        }
    }
    Ok(())
}

/// A diagnostic display inventory read from a text file on every poll.
///
/// Each nonblank line not starting with `#` describes one connected display
/// as tab-separated `key=value` fields: required `id` and `mode`, optional
/// `name` (defaults to the id), `x`, `y`, `modes` (`;`-separated, defaults
/// to the current mode), and `hdr` (`hdr`, `sdr`, or `unknown`). A missing
/// file means no display is connected, so a test unplugs every display by
/// removing it. Writers should replace the file atomically.
#[derive(Clone, Debug)]
pub(crate) struct InventoryFile {
    path: PathBuf,
}

impl InventoryFile {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl DisplayProvider for InventoryFile {
    fn displays(&mut self) -> Result<Vec<DisplayInfo>, DisplayEnumerationError> {
        read_inventory(&self.path)
    }
}

fn read_inventory(path: &Path) -> Result<Vec<DisplayInfo>, DisplayEnumerationError> {
    match fs::read_to_string(path) {
        Ok(text) => parse_inventory(&text),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(DisplayEnumerationError(format!(
            "display inventory unreadable: {}",
            error.kind()
        ))),
    }
}

pub(crate) fn parse_inventory(text: &str) -> Result<Vec<DisplayInfo>, DisplayEnumerationError> {
    let mut displays = Vec::new();
    let mut ids = BTreeSet::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fail = |detail: &str| {
            DisplayEnumerationError(format!("display inventory line {}: {detail}", number + 1))
        };
        let mut id = None;
        let mut name = None;
        let mut x = 0;
        let mut y = 0;
        let mut mode = None;
        let mut modes = None;
        let mut hdr = HdrCapability::Unknown;
        for field in line.split('\t') {
            let (key, value) = field
                .split_once('=')
                .ok_or_else(|| fail(&format!("field `{field}` is not key=value")))?;
            match key {
                "id" => id = Some(DisplayId::new(value).map_err(|error| fail(&error.to_string()))?),
                "name" => name = Some(value.to_owned()),
                "x" => x = value.parse().map_err(|_| fail("invalid x"))?,
                "y" => y = value.parse().map_err(|_| fail("invalid y"))?,
                "mode" => {
                    mode = Some(
                        value
                            .parse::<DisplayMode>()
                            .map_err(|error| fail(&error.to_string()))?,
                    );
                }
                "modes" => {
                    modes = Some(
                        value
                            .split(';')
                            .map(str::parse::<DisplayMode>)
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|error| fail(&error.to_string()))?,
                    );
                }
                "hdr" => {
                    hdr = match value {
                        "hdr" => HdrCapability::Hdr,
                        "sdr" => HdrCapability::Sdr,
                        "unknown" => HdrCapability::Unknown,
                        _ => return Err(fail("hdr must be hdr, sdr, or unknown")),
                    };
                }
                _ => return Err(fail(&format!("unknown key `{key}`"))),
            }
        }
        let id = id.ok_or_else(|| fail("missing id"))?;
        let mode = mode.ok_or_else(|| fail("missing mode"))?;
        if !ids.insert(id.clone()) {
            return Err(fail(&format!("duplicate id `{id}`")));
        }
        let mut info = DisplayInfo::new(id.clone(), name.unwrap_or_else(|| id.to_string()), mode);
        info.x = x;
        info.y = y;
        if let Some(modes) = modes {
            info.modes = modes;
        }
        info.hdr = hdr;
        displays.push(info);
    }
    Ok(displays)
}

#[cfg(feature = "native-media")]
pub(crate) use headless::InventoryDisplayHost;

#[cfg(feature = "native-media")]
mod headless {
    use std::{
        num::NonZeroU32,
        time::{Duration, Instant},
    };

    use fm_gpu::NativeTexture;
    use fm_io_api::display::{
        DisplayOutputSet, DisplayOutputSource, DisplayOutputTracker, DisplayProvider,
    };
    use freemixd::native_media::{NativeMediaRuntime, NativeProgramReadback};

    use super::{DISPLAY_FRAME_PREFIX, InventoryFile, record_text, status_record};

    const POLL_INTERVAL: Duration = Duration::from_millis(100);
    /// Every this many presented frames an output publishes one probe record.
    const PROBE_INTERVAL: u64 = 10;
    /// Larger diagnostic displays are presented at their real raster but not
    /// probed; this keeps the synchronous readback bounded.
    const MAX_PROBE_PIXELS: u64 = 4096 * 2160;

    struct HostOutput {
        target: Option<NativeProgramReadback>,
        presented: u64,
        probe_next: bool,
    }

    /// Presents each configured display output into an off-screen raster
    /// sized to its attached diagnostic display, with the output's own
    /// source and scaling, and follows inventory hot-plug through the shared
    /// tracker. Used with `--diagnostic-display-inventory` to verify
    /// independent sources and recovery without a window system.
    pub(crate) struct InventoryDisplayHost {
        tracker: DisplayOutputTracker,
        provider: InventoryFile,
        outputs: Vec<HostOutput>,
        polled_at: Option<Instant>,
        failures: u64,
    }

    impl InventoryDisplayHost {
        pub(crate) fn new(set: DisplayOutputSet, provider: InventoryFile) -> Self {
            let count = set.outputs().len();
            let tracker = DisplayOutputTracker::new(set, false);
            for index in 0..count {
                if let (Some(config), Some(state)) = (tracker.config(index), tracker.state(index)) {
                    eprintln!("{}", status_record(index, config, state));
                }
            }
            Self {
                tracker,
                provider,
                outputs: (0..count)
                    .map(|_| HostOutput {
                        target: None,
                        presented: 0,
                        probe_next: false,
                    })
                    .collect(),
                polled_at: None,
                failures: 0,
            }
        }

        /// Re-enumerates at most every poll interval and applies changes.
        pub(crate) fn poll(&mut self, runtime: &NativeMediaRuntime, now: Instant) {
            if self
                .polled_at
                .is_some_and(|polled| now.duration_since(polled) < POLL_INTERVAL)
            {
                return;
            }
            self.polled_at = Some(now);
            let changes = match self.provider.displays() {
                Ok(displays) => self.tracker.reconcile(&displays),
                Err(error) => {
                    eprintln!(
                        "FREEMIXD_DISPLAY_INVENTORY\tv=1\terror={}",
                        record_text(&error.0)
                    );
                    self.tracker.enumeration_failed()
                }
            };
            for change in changes {
                let Some(config) = self.tracker.config(change.output) else {
                    continue;
                };
                eprintln!("{}", status_record(change.output, config, &change.current));
                let output = &mut self.outputs[change.output];
                output.probe_next = true;
                let Some(active) = change.current.active() else {
                    output.target = None;
                    continue;
                };
                let reuse = output.target.as_ref().is_some_and(|target| {
                    (target.width(), target.height()) == (active.mode.width, active.mode.height)
                });
                if reuse {
                    continue;
                }
                output.target = match (
                    NonZeroU32::new(active.mode.width),
                    NonZeroU32::new(active.mode.height),
                ) {
                    (Some(width), Some(height)) => {
                        match runtime.create_program_readback_blocking(width, height) {
                            Ok(target) => Some(target),
                            Err(error) => {
                                self.failures = self.failures.saturating_add(1);
                                eprintln!(
                                    "FREEMIXD_DISPLAY_INVENTORY\tv=1\toutput={}\terror=target:{}",
                                    change.output + 1,
                                    record_text(&error.to_string())
                                );
                                None
                            }
                        }
                    }
                    _ => None,
                };
            }
        }

        /// Presents every attached output's own source with its own scaling.
        pub(crate) fn present<'a>(
            &mut self,
            runtime: &NativeMediaRuntime,
            resolve: impl Fn(DisplayOutputSource) -> Option<&'a NativeTexture>,
        ) {
            for (index, output) in self.outputs.iter_mut().enumerate() {
                let (Some(target), Some(config), Some(state)) = (
                    output.target.as_ref(),
                    self.tracker.config(index),
                    self.tracker.state(index),
                ) else {
                    continue;
                };
                let Some(active) = state.active() else {
                    continue;
                };
                let Some(texture) = resolve(config.source) else {
                    continue;
                };
                if runtime
                    .present_program_scaled_blocking(target, texture, config.scaling)
                    .is_err()
                {
                    self.failures = self.failures.saturating_add(1);
                    continue;
                }
                output.presented = output.presented.saturating_add(1);
                let pixels = u64::from(target.width()) * u64::from(target.height());
                if !(output.probe_next || output.presented.is_multiple_of(PROBE_INTERVAL))
                    || pixels > MAX_PROBE_PIXELS
                {
                    continue;
                }
                output.probe_next = false;
                let Ok(readback) = runtime.readback_presented_blocking(target) else {
                    self.failures = self.failures.saturating_add(1);
                    continue;
                };
                let pixel = |x: u32, y: u32| -> String {
                    let offset = usize::try_from(
                        u64::from(y) * u64::from(readback.stride) + u64::from(x) * 4,
                    )
                    .unwrap_or(usize::MAX);
                    readback
                        .rgba
                        .get(offset..offset.saturating_add(4))
                        .map_or_else(
                            || "none".to_owned(),
                            |rgba| format!("{},{},{},{}", rgba[0], rgba[1], rgba[2], rgba[3]),
                        )
                };
                eprintln!(
                    "{DISPLAY_FRAME_PREFIX}\tv=1\toutput={}\tdisplay={}\tsource={}\tscaling={}\tpresented={}\twidth={}\theight={}\tcorner={}\tcenter={}",
                    index + 1,
                    record_text(active.id.as_str()),
                    config.source,
                    config.scaling,
                    output.presented,
                    readback.width,
                    readback.height,
                    pixel(0, 0),
                    pixel(readback.width / 2, readback.height / 2),
                );
            }
        }

        /// Total frames presented by every output and failed GPU operations.
        pub(crate) fn totals(&self) -> (u64, u64) {
            (
                self.outputs
                    .iter()
                    .map(|output| output.presented)
                    .fold(0, u64::saturating_add),
                self.failures,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use fm_io_api::display::{
        ActiveDisplay, DisplayOutputTracker, DisplayProvider, HdrState, WaitReason,
    };

    use super::*;

    fn set(specs: &[&str]) -> DisplayOutputSet {
        DisplayOutputSet::new(specs.iter().map(|spec| spec.parse().unwrap()).collect()).unwrap()
    }

    #[test]
    fn inventory_lines_parse_every_field_and_reject_malformed_lines() {
        let displays = parse_inventory(
            "# two displays\n\
             id=left\tname=Left\tx=0\ty=0\tmode=64x36@60\tmodes=64x36@60;32x18@60\thdr=hdr\n\
             \n\
             id=right\tx=64\tmode=48x48\n",
        )
        .unwrap();
        assert_eq!(displays.len(), 2);
        assert_eq!(displays[0].name, "Left");
        assert_eq!(displays[0].modes.len(), 2);
        assert_eq!(displays[0].hdr, HdrCapability::Hdr);
        assert_eq!(displays[1].name, "right");
        assert_eq!(displays[1].x, 64);
        assert_eq!(displays[1].current_mode, DisplayMode::new(48, 48, 0));

        for bad in [
            "name=missing-id\tmode=1x1",
            "id=a",
            "id=a\tmode=1x1\tcolor=red",
            "id=a\tmode=1x1\nid=a\tmode=1x1",
            "id=a\tmode=1x1\thdr=dolby",
            "id=a mode=1x1",
        ] {
            assert!(parse_inventory(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_missing_inventory_file_means_every_display_is_unplugged() {
        let directory = tempfile::tempdir().unwrap();
        let mut file = InventoryFile::new(directory.path().join("displays.tsv"));
        assert_eq!(file.displays().unwrap(), Vec::new());
        fs::write(file.path(), "id=a\tmode=8x8\n").unwrap();
        assert_eq!(file.displays().unwrap().len(), 1);
    }

    #[test]
    fn status_records_describe_active_and_waiting_outputs() {
        let set = set(&["display=name:Left,source=output:2,scaling=fill"]);
        let config = &set.outputs()[0];
        let active = DisplayOutputState::Active(ActiveDisplay {
            id: DisplayId::new("x:1").unwrap(),
            name: "Left\tPanel".into(),
            x: 0,
            y: 0,
            mode: DisplayMode::new(1920, 1080, 60_000),
            mode_fallback: false,
            hdr: HdrState::Sdr,
        });
        assert_eq!(
            status_record(0, config, &active),
            "FREEMIXD_DISPLAY_OUTPUT\tv=1\toutput=1\tstate=active\tdisplay=x:1\tname=Left_Panel\t\
             mode=1920x1080@60000\tmode_fallback=false\thdr=sdr\tsource=output:2\tscaling=fill\t\
             borderless=true\tcursor=hidden"
        );
        assert_eq!(
            status_record(
                1,
                config,
                &DisplayOutputState::Waiting(WaitReason::DisplayAbsent)
            ),
            "FREEMIXD_DISPLAY_OUTPUT\tv=1\toutput=2\tstate=waiting\treason=display-absent\t\
             source=output:2\tscaling=fill\tborderless=true\tcursor=hidden"
        );
    }

    #[test]
    fn sources_must_be_program_or_a_declared_output() {
        let declared = [fm_types::OutputId::new(NonZeroU128::new(2).unwrap())];
        assert!(
            validate_sources(&set(&["display=0", "display=1,source=output:2"]), &declared).is_ok()
        );
        let preview = validate_sources(&set(&["display=0,source=preview"]), &declared).unwrap_err();
        assert!(preview.contains("source=preview"));
        let unknown =
            validate_sources(&set(&["display=0,source=output:9"]), &declared).unwrap_err();
        assert!(unknown.contains("output:9"));
    }

    #[test]
    fn inventory_file_hot_plug_drives_the_shared_tracker() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("displays.tsv");
        let mut file = InventoryFile::new(path.clone());
        let mut tracker = DisplayOutputTracker::new(set(&["display=id:a", "display=id:b"]), false);
        fs::write(&path, "id=a\tmode=8x8\nid=b\tx=8\tmode=8x8\n").unwrap();
        assert_eq!(tracker.reconcile(&file.displays().unwrap()).len(), 2);
        fs::write(&path, "id=b\tx=8\tmode=8x8\n").unwrap();
        let changes = tracker.reconcile(&file.displays().unwrap());
        assert_eq!(changes.len(), 1);
        assert!(changes[0].detached());
        fs::write(&path, "id=a\tmode=8x8\nid=b\tx=8\tmode=8x8\n").unwrap();
        let changes = tracker.reconcile(&file.displays().unwrap());
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].output, 0);
        assert!(changes[0].current.active().is_some());
    }
}
