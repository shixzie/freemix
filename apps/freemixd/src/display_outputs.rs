//! Fullscreen display outputs (OR-011): daemon status records, the
//! project-to-presenter synchronization, the wire projection of presenter
//! state, the diagnostic inventory-file display provider, and the headless
//! presenter that exercises the same scaled placement off-screen.
//!
//! The project's display-output slots (schema 40) are engine state edited
//! by `set_display_output` / `clear_display_output` (Protocol 2.42). After
//! every command and frame the daemon diffs them against what its presenter
//! shows ([`DisplayOutputSync`]) and applies the difference live: a source,
//! scaling, mode, or HDR change keeps the output on its display, and a new
//! display rebinds it. The presenter's per-slot state and display inventory
//! flow back as `display_output_status` / `display_inventory` runtime
//! events. A daemon without a presenter leaves every slot waiting with
//! [`fm_io_api::display::WaitReason::NoPresenter`].
//!
//! Real borderless windows are driven by `program_surface` (feature
//! `display-surface`); both presenters share the portable
//! [`fm_io_api::display::DisplayOutputTracker`] hot-plug state machine and
//! emit one `FREEMIXD_DISPLAY_OUTPUT` record per state change.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use fm_io_api::display::{
    DisplayEnumerationError, DisplayId, DisplayInfo, DisplayMode, DisplayOutputConfig,
    DisplayOutputSlot, DisplayOutputState, DisplayProvider, HdrCapability, MAX_DISPLAY_TEXT_BYTES,
};
use fm_protocol::{
    DisplayInventory, DisplayInventoryEntry, DisplayOutputEntry, MAX_DISPLAY_INVENTORY_ERROR_BYTES,
    MAX_INVENTORY_DISPLAY_MODES, MAX_INVENTORY_DISPLAYS, WaitReason, WireActiveDisplay,
    WireDisplayOutputState,
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

/// One `FREEMIXD_DISPLAY_OUTPUT` record for output `index` (zero-based slot
/// position).
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

/// The `FREEMIXD_DISPLAY_OUTPUT` record of a slot that was cleared live.
pub(crate) fn cleared_record(index: usize) -> String {
    format!(
        "{DISPLAY_OUTPUT_PREFIX}\tv=1\toutput={}\tstate=cleared",
        index + 1
    )
}

/// What a display presenter last observed: each configured slot's state
/// and the connected displays.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DisplayReport {
    pub(crate) states: BTreeMap<DisplayOutputSlot, DisplayOutputState>,
    pub(crate) displays: Vec<DisplayInfo>,
    /// Why the last enumeration failed, if it did.
    pub(crate) error: Option<String>,
}

/// The longest prefix of `value` with at most `maximum` UTF-8 bytes.
fn bounded(value: &str, maximum: usize) -> &str {
    let mut end = maximum.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Platform display text as the wire carries it: commas and control
/// characters replaced and the length bounded; `fallback` when empty.
fn wire_display_text(value: &str, fallback: &DisplayId) -> String {
    let cleaned = value
        .chars()
        .map(|c| if c.is_control() || c == ',' { '_' } else { c })
        .collect::<String>();
    let text = bounded(&cleaned, MAX_DISPLAY_TEXT_BYTES);
    if text.is_empty() {
        fallback.to_string()
    } else {
        text.to_owned()
    }
}

/// One slot's presenter state on the wire.
pub(crate) fn wire_state(state: &DisplayOutputState) -> WireDisplayOutputState {
    match state {
        DisplayOutputState::Waiting(reason) => WireDisplayOutputState::Waiting(*reason),
        DisplayOutputState::Active(active) => WireDisplayOutputState::Active(WireActiveDisplay {
            id: active.id.clone(),
            name: wire_display_text(&active.name, &active.id),
            mode: active.mode,
            mode_fallback: active.mode_fallback,
            hdr: active.hdr,
        }),
    }
}

/// The presenter's inventory on the wire, bounded to what the protocol
/// carries: at most [`MAX_INVENTORY_DISPLAYS`] displays with valid modes, at
/// most [`MAX_INVENTORY_DISPLAY_MODES`] modes each (largest first).
pub(crate) fn wire_inventory(report: &DisplayReport) -> DisplayInventory {
    let displays = report
        .displays
        .iter()
        .filter(|display| display.current_mode.is_valid())
        .take(MAX_INVENTORY_DISPLAYS)
        .map(|display| {
            let mut modes = display
                .modes
                .iter()
                .copied()
                .filter(|mode| mode.is_valid())
                .collect::<Vec<_>>();
            modes.sort_unstable_by(|left, right| right.cmp(left));
            modes.dedup();
            modes.truncate(MAX_INVENTORY_DISPLAY_MODES);
            DisplayInventoryEntry {
                id: display.id.clone(),
                name: wire_display_text(&display.name, &display.id),
                x: display.x,
                y: display.y,
                current_mode: display.current_mode,
                modes,
                hdr: display.hdr,
            }
        })
        .collect();
    let error = report
        .error
        .as_deref()
        .map(|error| record_text(bounded(error, MAX_DISPLAY_INVENTORY_ERROR_BYTES)))
        .filter(|error| !error.is_empty());
    DisplayInventory {
        presenter: true,
        error,
        displays,
    }
}

/// Keeps a presenter showing the project's display-output slots and the
/// control service told about the presenter's state.
#[derive(Debug, Default)]
pub(crate) struct DisplayOutputSync {
    applied: BTreeMap<DisplayOutputSlot, DisplayOutputConfig>,
}

impl DisplayOutputSync {
    /// The slot edits that bring the presenter from what it was last told to
    /// `desired` (the snapshot's configured slots), recording them as
    /// applied.
    pub(crate) fn changes(
        &mut self,
        desired: &[DisplayOutputEntry],
    ) -> Vec<(DisplayOutputSlot, Option<DisplayOutputConfig>)> {
        let desired = desired
            .iter()
            .map(|entry| (entry.slot, &entry.config))
            .collect::<BTreeMap<_, _>>();
        let mut changes = Vec::new();
        for slot in DisplayOutputSlot::ALL {
            let wanted = desired.get(&slot).copied();
            if self.applied.get(&slot) == wanted {
                continue;
            }
            match wanted {
                Some(config) => {
                    self.applied.insert(slot, config.clone());
                }
                None => {
                    self.applied.remove(&slot);
                }
            }
            changes.push((slot, wanted.cloned()));
        }
        // A display moving between slots is cleared from its old slot first
        // so the presenter never sees it claimed twice.
        changes.sort_by_key(|(_, config)| config.is_some());
        changes
    }

    /// Every slot's state as the presenter reports it: configured slots
    /// from `report` (a slot it has not configured yet waits for it), and
    /// unconfigured slots waiting for a presenter.
    pub(crate) fn wire_states(
        &self,
        report: &DisplayReport,
    ) -> Vec<(DisplayOutputSlot, WireDisplayOutputState)> {
        DisplayOutputSlot::ALL
            .into_iter()
            .map(|slot| {
                let state = match (self.applied.contains_key(&slot), report.states.get(&slot)) {
                    (true, Some(state)) => wire_state(state),
                    (true, None) => WireDisplayOutputState::Waiting(WaitReason::DisplayAbsent),
                    (false, _) => WireDisplayOutputState::Waiting(WaitReason::NoPresenter),
                };
                (slot, state)
            })
            .collect()
    }
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
                    hdr = value
                        .parse::<HdrCapability>()
                        .map_err(|_| fail("hdr must be hdr, sdr, or unknown"))?;
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
        DisplayOutputConfig, DisplayOutputReconfiguration, DisplayOutputSlot, DisplayOutputSource,
        DisplayOutputTracker, DisplayProvider, MAX_DISPLAY_OUTPUTS,
    };
    use freemixd::native_media::{NativeMediaRuntime, NativeProgramReadback};

    use super::{
        DISPLAY_FRAME_PREFIX, DisplayReport, InventoryFile, cleared_record, record_text,
        status_record,
    };

    const POLL_INTERVAL: Duration = Duration::from_millis(100);
    /// Every this many presented frames an output publishes one probe record.
    const PROBE_INTERVAL: u64 = 10;
    /// Larger diagnostic displays are presented at their real raster but not
    /// probed; this keeps the synchronous readback bounded.
    const MAX_PROBE_PIXELS: u64 = 4096 * 2160;

    #[derive(Default)]
    struct HostOutput {
        target: Option<NativeProgramReadback>,
        presented: u64,
        probe_next: bool,
    }

    /// Presents each configured display-output slot into an off-screen
    /// raster sized to its attached diagnostic display, with the slot's own
    /// source and scaling, and follows inventory hot-plug through the
    /// shared tracker. Used with `--diagnostic-display-inventory` to verify
    /// independent sources, live reconfiguration, and recovery without a
    /// window system.
    pub(crate) struct InventoryDisplayHost {
        tracker: DisplayOutputTracker,
        provider: InventoryFile,
        outputs: Vec<HostOutput>,
        polled_at: Option<Instant>,
        failures: u64,
        report: DisplayReport,
    }

    impl InventoryDisplayHost {
        /// A host with every slot empty; [`Self::configure`] fills them.
        pub(crate) fn new(provider: InventoryFile) -> Self {
            Self {
                // The headless presenter is SDR-only, like the windowed one.
                tracker: DisplayOutputTracker::empty(false),
                provider,
                outputs: (0..MAX_DISPLAY_OUTPUTS)
                    .map(|_| HostOutput::default())
                    .collect(),
                polled_at: None,
                failures: 0,
                report: DisplayReport::default(),
            }
        }

        /// Fills, retargets, or clears one slot live; the next poll binds
        /// a slot that waits for its display.
        pub(crate) fn configure(
            &mut self,
            slot: DisplayOutputSlot,
            config: Option<DisplayOutputConfig>,
        ) {
            let index = slot.index();
            match self.tracker.configure(index, config) {
                DisplayOutputReconfiguration::Unchanged => return,
                DisplayOutputReconfiguration::Retargeted => {
                    self.outputs[index].probe_next = true;
                }
                DisplayOutputReconfiguration::Rebinding => {
                    self.outputs[index] = HostOutput::default();
                    if let (Some(config), Some(state)) =
                        (self.tracker.config(index), self.tracker.state(index))
                    {
                        eprintln!("{}", status_record(index, config, state));
                    }
                }
                DisplayOutputReconfiguration::Cleared => {
                    self.outputs[index] = HostOutput::default();
                    eprintln!("{}", cleared_record(index));
                }
            }
            // Re-enumerate at once so the slot binds on this frame.
            self.polled_at = None;
            self.refresh_states();
        }

        fn refresh_states(&mut self) {
            self.report.states = DisplayOutputSlot::ALL
                .into_iter()
                .filter_map(|slot| {
                    self.tracker
                        .state(slot.index())
                        .map(|state| (slot, state.clone()))
                })
                .collect();
        }

        /// What the host last observed.
        pub(crate) const fn report(&self) -> &DisplayReport {
            &self.report
        }

        /// Whether a configured slot shows `source`.
        pub(crate) fn shows(&self, source: DisplayOutputSource) -> bool {
            (0..MAX_DISPLAY_OUTPUTS)
                .filter_map(|index| self.tracker.config(index))
                .any(|config| config.source == source)
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
                Ok(displays) => {
                    let changes = self.tracker.reconcile(&displays);
                    self.report.displays = displays;
                    self.report.error = None;
                    changes
                }
                Err(error) => {
                    eprintln!(
                        "FREEMIXD_DISPLAY_INVENTORY\tv=1\terror={}",
                        record_text(&error.0)
                    );
                    self.report.displays.clear();
                    self.report.error = Some(error.0);
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
            self.refresh_states();
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
    use fm_io_api::display::{
        ActiveDisplay, DisplayOutputSet, DisplayOutputTracker, DisplayProvider, HdrState,
    };

    use super::*;

    fn set(specs: &[&str]) -> DisplayOutputSet {
        DisplayOutputSet::new(specs.iter().map(|spec| spec.parse().unwrap()).collect()).unwrap()
    }

    fn slot(slot: u8) -> DisplayOutputSlot {
        DisplayOutputSlot::new(slot).unwrap()
    }

    fn entry(number: u8, spec: &str) -> DisplayOutputEntry {
        DisplayOutputEntry {
            slot: slot(number),
            config: spec.parse().unwrap(),
        }
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
    fn status_records_describe_active_waiting_and_cleared_outputs() {
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
        assert_eq!(
            cleared_record(1),
            "FREEMIXD_DISPLAY_OUTPUT\tv=1\toutput=2\tstate=cleared"
        );
    }

    #[test]
    fn sync_applies_only_changed_slots_and_clears_before_it_fills() {
        let mut sync = DisplayOutputSync::default();
        let changes = sync.changes(&[entry(1, "display=index:0"), entry(2, "display=index:1")]);
        assert_eq!(changes.len(), 2);
        assert_eq!(
            sync.changes(&[entry(1, "display=index:0"), entry(2, "display=index:1")]),
            Vec::new()
        );
        // Swapping the displays clears nothing but changes both; moving one
        // display from slot 2 to slot 1 clears slot 2 first.
        let changes = sync.changes(&[entry(1, "display=index:1")]);
        assert_eq!(changes[0], (slot(2), None));
        assert_eq!(changes[1].0, slot(1));
        assert!(changes[1].1.is_some());

        let report = DisplayReport {
            states: BTreeMap::from([(
                slot(1),
                DisplayOutputState::Waiting(WaitReason::DisplayInUse),
            )]),
            ..DisplayReport::default()
        };
        assert_eq!(
            sync.wire_states(&report),
            vec![
                (
                    slot(1),
                    WireDisplayOutputState::Waiting(WaitReason::DisplayInUse)
                ),
                (
                    slot(2),
                    WireDisplayOutputState::Waiting(WaitReason::NoPresenter)
                ),
            ]
        );
    }

    #[test]
    fn the_wire_inventory_is_bounded_and_sanitized() {
        let id = DisplayId::new("x:1").unwrap();
        let mut display =
            DisplayInfo::new(id.clone(), "Left, Panel\n", DisplayMode::new(64, 36, 0));
        display.modes = (1..=200)
            .map(|width| DisplayMode::new(width, 36, 0))
            .collect();
        display.modes.push(DisplayMode::new(0, 0, 0));
        let mut unnamed = DisplayInfo::new(
            DisplayId::new("x:2").unwrap(),
            "",
            DisplayMode::new(32, 32, 0),
        );
        unnamed.hdr = HdrCapability::Hdr;
        let report = DisplayReport {
            states: BTreeMap::new(),
            displays: std::iter::repeat_n(display, 20)
                .chain(std::iter::once(unnamed))
                .collect(),
            error: Some("x".repeat(400)),
        };
        let inventory = wire_inventory(&report);
        assert!(inventory.presenter);
        assert_eq!(inventory.displays.len(), MAX_INVENTORY_DISPLAYS);
        assert_eq!(inventory.displays[0].name, "Left_ Panel_");
        assert_eq!(
            inventory.displays[0].modes.len(),
            MAX_INVENTORY_DISPLAY_MODES
        );
        assert_eq!(inventory.displays[0].modes[0], DisplayMode::new(200, 36, 0));
        assert_eq!(
            inventory.error.as_deref().map(str::len),
            Some(MAX_DISPLAY_INVENTORY_ERROR_BYTES)
        );
        let line = fm_protocol::encode_line(&fm_protocol::WireMessage::DisplayInventory(
            fm_protocol::DisplayInventoryMessage {
                request_id: "r".into(),
                inventory: inventory.clone(),
            },
        ))
        .unwrap();
        assert!(line.len() < 64 * 1024);
        assert_eq!(
            wire_display_text("", &id),
            "x:1",
            "an unnamed display shows its id"
        );
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
