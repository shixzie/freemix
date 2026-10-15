//! Input lifecycle and organisation in the bank (Protocol 2.38, IN-026).
//!
//! Pure helpers pick the colour-label palette, the bank's category filter,
//! the tile badges (offline, closed, paused), fresh input identifiers for
//! duplicates and batch imports, the pasted path list of a batch import,
//! and the category list checks. The drawing half adds the bank toolbar
//! (filter chips, Import files…, Categories…), the lifecycle items of the
//! tile menu, and the Replace source, Import files, and Categories dialogs.
//!
//! Every action becomes one [`StudioIntent::InputLifecycle`]. The filter is
//! local to this console and never a command; tiles keep their real bank
//! position (and number key) while others are hidden.

use egui::{Button, Color32, RichText, Sense, Stroke, Ui, Vec2};
use fm_protocol::{
    ImportItemSpec, InputCategoryStatus, InputColorLabel, InputLifecycleOperation,
    InputLifecycleStatus, InputOfflineReason, MAX_BATCH_IMPORT_ITEMS, MAX_INPUT_CATEGORIES,
    MAX_INPUT_CATEGORY_NAME_BYTES, MAX_INPUT_SOURCE_BYTES, WireGeneratorVideo, WireInputId,
    WireInputSource,
};
use fm_types::InputId;
use fm_ui_model::ClientView;

use crate::edit_history::PROJECT_LOCKED_REASON;
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::input_bank::project_editing_available;
use crate::input_catalogue::{
    COLOUR_PRESETS, CatalogueAvailability, MAX_LIVE_INPUTS, ON_PROGRAM_REASON, capitalized,
    colour_swatch, hex_colour, kind_key, next_free_input_id,
};
use crate::l10n::{self, Arg, Locale, Msg, with_key};
use crate::theme::{
    ACCENT_FILL, AMBER, ERROR, Tone, accent_color, mono, muted_color, paint_lamp, section_label,
    small_button, text_color, text_dim_color,
};
use crate::{StudioIntent, StudioShell, StudioUiState};

/// The tile colour of a colour label (the same RGB the project model
/// documents for each label).
#[must_use]
pub(crate) const fn label_color(label: InputColorLabel) -> Color32 {
    match label {
        InputColorLabel::Red => Color32::from_rgb(0xE5, 0x48, 0x4D),
        InputColorLabel::Orange => Color32::from_rgb(0xF7, 0x6B, 0x15),
        InputColorLabel::Yellow => Color32::from_rgb(0xF5, 0xD9, 0x0A),
        InputColorLabel::Green => Color32::from_rgb(0x30, 0xA4, 0x6C),
        InputColorLabel::Teal => Color32::from_rgb(0x12, 0xA5, 0x94),
        InputColorLabel::Blue => Color32::from_rgb(0x00, 0x90, 0xFF),
        InputColorLabel::Purple => Color32::from_rgb(0x8E, 0x4E, 0xC6),
        InputColorLabel::Pink => Color32::from_rgb(0xD6, 0x40, 0x9F),
    }
}

/// The operator name of a colour label.
#[must_use]
pub(crate) const fn label_name(label: InputColorLabel) -> Msg {
    match label {
        InputColorLabel::Red => Msg::ColourRed,
        InputColorLabel::Orange => Msg::LcColourOrange,
        InputColorLabel::Yellow => Msg::LcColourYellow,
        InputColorLabel::Green => Msg::ColourGreen,
        InputColorLabel::Teal => Msg::LcColourTeal,
        InputColorLabel::Blue => Msg::ColourBlue,
        InputColorLabel::Purple => Msg::LcColourPurple,
        InputColorLabel::Pink => Msg::LcColourPink,
    }
}

/// Which inputs the bank shows. Local to this console.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BankFilter {
    #[default]
    All,
    /// Inputs without a category.
    Uncategorised,
    /// Inputs filed under one category id.
    Category(u32),
}

impl BankFilter {
    /// Whether an input with `lifecycle` passes the filter.
    #[must_use]
    pub(crate) fn shows(self, lifecycle: InputLifecycleStatus) -> bool {
        match self {
            Self::All => true,
            Self::Uncategorised => lifecycle.category.is_none(),
            Self::Category(id) => lifecycle.category == Some(id),
        }
    }

    /// The filter after the category list changed: a category that no
    /// longer exists falls back to All, and so does Uncategorised once the
    /// project has no categories (its chip is gone).
    #[must_use]
    pub(crate) fn reconciled(self, categories: &[InputCategoryStatus]) -> Self {
        match self {
            Self::Category(id) if !categories.iter().any(|category| category.id == id) => Self::All,
            Self::Uncategorised if categories.is_empty() => Self::All,
            filter => filter,
        }
    }
}

/// Bank positions (indices into [`ClientView::inputs`]) the filter shows,
/// in bank order. Tiles keep their real position, so number keys and the
/// tile key caps still name the same input whatever is hidden.
#[must_use]
pub(crate) fn visible_inputs(view: &ClientView, filter: BankFilter) -> Vec<usize> {
    (0..view.inputs.len())
        .filter(|index| {
            filter.shows(
                view.input_lifecycle
                    .get(*index)
                    .copied()
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// A status badge painted on an input tile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LifecycleBadge {
    Offline(InputOfflineReason),
    Closed,
    Paused,
}

impl LifecycleBadge {
    /// The badge text in the console language.
    pub(crate) fn text(self, l: Locale) -> &'static str {
        l.text(match self {
            Self::Offline(_) => Msg::LcBadgeOffline,
            Self::Closed => Msg::LcBadgeClosed,
            Self::Paused => Msg::PlaylistStatePaused,
        })
    }

    /// The badge's tile hover wording: offline names its reason token.
    pub(crate) fn detail(self, l: Locale) -> String {
        match self {
            Self::Offline(reason) => l.format(
                Msg::LcOfflineDetail,
                &[
                    ("badge", Arg::Str(self.text(l))),
                    ("reason", Arg::Str(reason.token())),
                ],
            ),
            Self::Closed | Self::Paused => self.text(l).to_owned(),
        }
    }

    pub(crate) const fn color(self) -> Color32 {
        match self {
            Self::Offline(_) => ERROR,
            Self::Closed => Color32::from_rgb(96, 102, 112),
            Self::Paused => AMBER,
        }
    }
}

/// The badges of one input, most severe first: an offline source, then a
/// closed input, then a paused one.
#[must_use]
pub(crate) fn lifecycle_badges(status: InputLifecycleStatus) -> Vec<LifecycleBadge> {
    let mut badges = Vec::new();
    if let Some(reason) = status.offline {
        badges.push(LifecycleBadge::Offline(reason));
    }
    if status.closed {
        badges.push(LifecycleBadge::Closed);
    }
    if status.paused {
        badges.push(LifecycleBadge::Paused);
    }
    badges
}

/// The first line of a tile's hover: its name, its category, and each
/// badge's detail, joined by ` · `.
#[must_use]
pub(crate) fn tile_status_line(
    l: Locale,
    name: &str,
    status: InputLifecycleStatus,
    categories: &[InputCategoryStatus],
) -> String {
    let mut parts = vec![name.to_owned()];
    if let Some(category) = status
        .category
        .and_then(|id| categories.iter().find(|category| category.id == id))
    {
        parts.push(category.name.clone());
    }
    parts.extend(
        lifecycle_badges(status)
            .into_iter()
            .map(|badge| badge.detail(l)),
    );
    parts.join(" · ")
}

/// Splits a pasted path list: one path per line, trimmed, surrounding
/// quotes (as file managers copy them) removed, blank lines and repeats
/// skipped.
#[must_use]
pub(crate) fn parse_import_paths(text: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut path = line.trim();
        for quote in ['"', '\''] {
            if path.len() >= 2 && path.starts_with(quote) && path.ends_with(quote) {
                path = path[1..path.len() - 1].trim();
            }
        }
        if !path.is_empty() && !paths.iter().any(|seen| seen == path) {
            paths.push(path.to_owned());
        }
    }
    paths
}

/// Why a batch import of `paths` into a show of `inputs` inputs (counting
/// adds in flight) would be refused before sending, if it would be.
#[must_use]
pub(crate) fn import_problem(paths: &[String], inputs: usize) -> Option<&'static str> {
    if paths.is_empty() {
        Some(Msg::LcReasonNoPaths.en())
    } else if paths.len() > MAX_BATCH_IMPORT_ITEMS {
        Some(Msg::LcReasonTooManyItems.en())
    } else if inputs + paths.len() > MAX_LIVE_INPUTS {
        Some(Msg::LcReasonImportOverLimit.en())
    } else if paths.iter().any(|path| path.len() > MAX_INPUT_SOURCE_BYTES) {
        Some(Msg::LcReasonPathTooLong.en())
    } else {
        None
    }
}

/// `count` fresh input identifiers, each one past the largest in `taken`
/// or already picked, as the Add input dialog picks one.
#[must_use]
pub(crate) fn allocate_input_ids(taken: &[InputId], count: usize) -> Option<Vec<InputId>> {
    let mut taken = taken.to_vec();
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        let id = next_free_input_id(&taken)?;
        taken.push(id);
        ids.push(id);
    }
    Some(ids)
}

/// The next category id: one past the largest in use (ids are nonzero and
/// client-chosen).
#[must_use]
pub(crate) fn next_category_id<'a>(
    ids: impl IntoIterator<Item = &'a InputCategoryStatus>,
) -> Option<u32> {
    ids.into_iter()
        .map(|category| category.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
}

/// Why `name` cannot name a category beside `others`, if it cannot.
/// Names are unique case-insensitively.
#[must_use]
pub(crate) fn category_name_problem<'a>(
    name: &str,
    others: impl IntoIterator<Item = &'a str>,
) -> Option<&'static str> {
    let name = name.trim();
    if name.is_empty() {
        Some(Msg::ReasonNameEmpty.en())
    } else if name.len() > MAX_INPUT_CATEGORY_NAME_BYTES {
        Some(Msg::LcReasonCategoryNameTooLong.en())
    } else if others
        .into_iter()
        .any(|other| other.trim().to_lowercase() == name.to_lowercase())
    {
        Some(Msg::LcReasonCategoryTaken.en())
    } else {
        None
    }
}

/// Why a category list draft cannot be saved over `current`, if it cannot.
#[must_use]
pub(crate) fn categories_problem(
    draft: &[InputCategoryStatus],
    current: &[InputCategoryStatus],
) -> Option<&'static str> {
    if draft.len() > MAX_INPUT_CATEGORIES {
        return Some(Msg::LcReasonCategoryLimit.en());
    }
    for (index, row) in draft.iter().enumerate() {
        let others = draft
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, other)| other.name.as_str());
        if let Some(problem) = category_name_problem(&row.name, others) {
            return Some(problem);
        }
    }
    let trimmed: Vec<InputCategoryStatus> = trimmed_categories(draft);
    (trimmed == current).then_some(Msg::LcReasonCategoriesUnchanged.en())
}

fn trimmed_categories(draft: &[InputCategoryStatus]) -> Vec<InputCategoryStatus> {
    draft
        .iter()
        .map(|row| InputCategoryStatus {
            id: row.id,
            name: row.name.trim().to_owned(),
        })
        .collect()
}

/// Whether any input is filed under category `id`.
#[must_use]
pub(crate) fn category_in_use(view: &ClientView, id: u32) -> bool {
    view.input_lifecycle
        .iter()
        .any(|lifecycle| lifecycle.category == Some(id))
}

/// Why project-wide lifecycle edits (the category list) are unavailable.
#[must_use]
pub(crate) fn project_edit_reason(state: &StudioUiState) -> Option<&'static str> {
    if !project_editing_available(state) {
        return Some(unavailable_reason(state, Capability::EditProject));
    }
    state
        .view
        .as_ref()
        .filter(|view| view.edit_locks.project)
        .map(|_| PROJECT_LOCKED_REASON)
}

/// The kind of a replacement source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceKind {
    Solid,
    Bars,
    Media,
    Device,
    Network,
}

impl SourceKind {
    const ALL: [Self; 5] = [
        Self::Solid,
        Self::Bars,
        Self::Media,
        Self::Device,
        Self::Network,
    ];

    const fn key(self) -> Msg {
        match self {
            Self::Solid => Msg::LcSourceSolid,
            Self::Bars => Msg::LcSourceBars,
            Self::Media => Msg::LcSourceMedia,
            Self::Device => Msg::LcSourceDevice,
            Self::Network => Msg::LcSourceNetwork,
        }
    }

    const fn hover(self) -> Msg {
        match self {
            Self::Solid => Msg::LcSourceSolidHover,
            Self::Bars => Msg::LcSourceBarsHover,
            Self::Media => Msg::LcSourceMediaHover,
            Self::Device => Msg::LcSourceDeviceHover,
            Self::Network => Msg::LcSourceNetworkHover,
        }
    }

    /// The hover of the kind's text field, for kinds that have one.
    const fn field_hover(self) -> Option<Msg> {
        match self {
            Self::Solid | Self::Bars => None,
            Self::Media => Some(Msg::LcSourceMediaFieldHover),
            Self::Device => Some(Msg::LcSourceDeviceFieldHover),
            Self::Network => Some(Msg::LcSourceNetworkFieldHover),
        }
    }
}

/// The open Replace source dialog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReplaceDraft {
    pub(crate) input: InputId,
    pub(crate) name: String,
    pub(crate) kind: SourceKind,
    pub(crate) color: Color32,
    /// The media path, device key, or network endpoint.
    pub(crate) text: String,
}

impl ReplaceDraft {
    pub(crate) fn new(input: InputId, name: &str) -> Self {
        Self {
            input,
            name: name.to_owned(),
            kind: SourceKind::Solid,
            color: COLOUR_PRESETS[5].1,
            text: String::new(),
        }
    }

    /// The replacement source, or why it cannot be sent.
    pub(crate) fn source(&self) -> Result<WireInputSource, &'static str> {
        let text = self.text.trim();
        let checked = || {
            if text.is_empty() {
                Err(Msg::LcReasonSourceEmpty.en())
            } else if text.len() > MAX_INPUT_SOURCE_BYTES {
                Err(Msg::LcReasonSourceTooLong.en())
            } else {
                Ok(text.to_owned())
            }
        };
        Ok(match self.kind {
            SourceKind::Solid => WireInputSource::Generator {
                video: WireGeneratorVideo::Solid {
                    rgba: [self.color.r(), self.color.g(), self.color.b(), 255],
                },
                tone_hz: None,
            },
            SourceKind::Bars => WireInputSource::Generator {
                video: WireGeneratorVideo::Bars,
                tone_hz: None,
            },
            SourceKind::Media => WireInputSource::Media { uri: checked()? },
            SourceKind::Device => WireInputSource::Device {
                stable_key: checked()?,
            },
            SourceKind::Network => WireInputSource::Network {
                endpoint: checked()?,
            },
        })
    }
}

/// The open Categories dialog: the edited list and the new-category field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CategoriesDraft {
    pub(crate) rows: Vec<InputCategoryStatus>,
    pub(crate) new_name: String,
}

/// The open Import files dialog.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ImportDraft {
    pub(crate) text: String,
    pub(crate) focus_pending: bool,
}

/// Console-local lifecycle state: the bank filter and the open dialogs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LifecyclePanel {
    pub(crate) filter: BankFilter,
    pub(crate) replace: Option<ReplaceDraft>,
    pub(crate) categories: Option<CategoriesDraft>,
    pub(crate) import: Option<ImportDraft>,
    /// The largest identifier this console handed to a duplicate or import,
    /// so a second request sent before the first replicates never reuses it.
    pub(crate) reserved: Option<InputId>,
}

impl LifecyclePanel {
    /// Whether a lifecycle dialog is open (and owns the keyboard).
    pub(crate) const fn dialog_open(&self) -> bool {
        self.replace.is_some() || self.categories.is_some() || self.import.is_some()
    }
}

/// The lifecycle facts of one tile its menu needs.
#[derive(Clone, Copy)]
pub(crate) struct LifecycleMenu<'a> {
    pub(crate) input: InputId,
    pub(crate) name: &'a str,
    /// Whether this input may be edited (session, padlocks), and why not.
    pub(crate) edit: (bool, &'static str),
    /// The right-column `LOCKED` mark when a padlock blocks the edit.
    pub(crate) mark: &'static str,
}

/// One tile-menu item with its hover and disabled reason.
fn menu_item(
    ui: &mut Ui,
    enabled: bool,
    button: Button<'_>,
    mark: &str,
    hover: &str,
    reason: &str,
) -> bool {
    let l = l10n::locale(ui.ctx());
    let button = if mark.is_empty() {
        button
    } else {
        button.shortcut_text(
            RichText::new(l.tr(mark))
                .size(crate::theme::legible(10.0))
                .strong()
                .color(AMBER),
        )
    };
    ui.add_enabled(enabled, button)
        .hint(hover, reason)
        .clicked()
}

impl StudioShell {
    /// Picks `count` fresh identifiers past every input, add in flight, and
    /// identifier this console already handed out.
    pub(crate) fn allocate_lifecycle_ids(
        &mut self,
        state: &StudioUiState,
        count: usize,
    ) -> Option<Vec<InputId>> {
        let view = state.view.as_ref()?;
        let mut taken = self.catalogue.taken_ids(view, state);
        taken.extend(self.lifecycle.reserved);
        let ids = allocate_input_ids(&taken, count)?;
        if let Some(last) = ids.last() {
            self.lifecycle.reserved = Some(*last);
        }
        Some(ids)
    }

    /// Duplicate and Import availability: Add input's gate.
    pub(crate) fn lifecycle_add_gate(&self, state: &StudioUiState) -> CatalogueAvailability {
        self.add_input_gate(state)
    }

    /// Drops dialogs and filters the latest state invalidated.
    pub(crate) fn reconcile_lifecycle(&mut self, state: &StudioUiState) {
        let Some(view) = state.view.as_ref() else {
            self.lifecycle.replace = None;
            self.lifecycle.categories = None;
            self.lifecycle.import = None;
            return;
        };
        self.lifecycle.filter = self.lifecycle.filter.reconciled(&view.input_categories);
        if self
            .lifecycle
            .replace
            .as_ref()
            .is_some_and(|draft| !view.inputs.contains(&draft.input))
        {
            self.lifecycle.replace = None;
        }
    }

    pub(crate) fn open_replace_source(&mut self, input: InputId, name: &str) {
        self.lifecycle.replace = Some(ReplaceDraft::new(input, name));
    }

    pub(crate) fn open_categories(&mut self, state: &StudioUiState) {
        if project_edit_reason(state).is_none()
            && let Some(view) = state.view.as_ref()
        {
            self.lifecycle.categories = Some(CategoriesDraft {
                rows: view.input_categories.clone(),
                new_name: String::new(),
            });
        }
    }

    pub(crate) fn open_import(&mut self, state: &StudioUiState) {
        if self.lifecycle_add_gate(state).enabled {
            self.lifecycle.import = Some(ImportDraft {
                text: String::new(),
                focus_pending: true,
            });
        }
    }

    /// The bank toolbar: category filter chips, then Import files… and
    /// Categories… on the trailing side.
    pub(crate) fn draw_bank_toolbar(&mut self, ui: &mut Ui, state: &StudioUiState) {
        let Some(view) = state.view.as_ref() else {
            return;
        };
        let l = self.locale;
        let note = l.text(Msg::LcFilterKeysNote);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let mut chip = |ui: &mut Ui, filter: BankFilter, label: &str, hover: String| {
                let selected = self.lifecycle.filter == filter;
                if ui
                    .add(
                        Button::selectable(
                            selected,
                            RichText::new(label).size(crate::theme::legible(10.5)),
                        )
                        .min_size(Vec2::new(0.0, 20.0)),
                    )
                    .on_hover_text(format!("{hover}\n{note}"))
                    .clicked()
                {
                    self.lifecycle.filter = filter;
                }
            };
            chip(
                ui,
                BankFilter::All,
                l.text(Msg::LcFilterAll),
                l.text(Msg::LcFilterAllHover).to_owned(),
            );
            if !view.input_categories.is_empty() {
                for category in &view.input_categories {
                    chip(
                        ui,
                        BankFilter::Category(category.id),
                        &category.name,
                        l.format(
                            Msg::LcFilterCategoryHover,
                            &[("category", Arg::Str(&category.name))],
                        ),
                    );
                }
                chip(
                    ui,
                    BankFilter::Uncategorised,
                    l.text(Msg::LcFilterUncategorised),
                    l.text(Msg::LcFilterUncategorisedHover).to_owned(),
                );
            }
            if self.lifecycle.filter != BankFilter::All {
                let shown = visible_inputs(view, self.lifecycle.filter).len();
                ui.label(
                    mono(l.format(
                        Msg::LcFilterShowing,
                        &[
                            ("shown", Arg::from(shown)),
                            ("total", Arg::from(view.inputs.len())),
                        ],
                    ))
                    .size(crate::theme::legible(10.0))
                    .color(muted_color()),
                );
            }
            ui.with_layout(l10n::trailing_layout(l), |ui| {
                let categories = project_edit_reason(state);
                if ui
                    .add_enabled(
                        categories.is_none(),
                        small_button(l.text(Msg::LcCategoriesEllipsis)),
                    )
                    .hint(
                        l.text(Msg::LcManageCategoriesHover),
                        categories.unwrap_or(""),
                    )
                    .clicked()
                {
                    self.open_categories(state);
                }
                let import = self.lifecycle_add_gate(state);
                if ui
                    .add_enabled(import.enabled, small_button(l.text(Msg::LcImportEllipsis)))
                    .hint(l.text(Msg::LcImportHover), import.reason)
                    .clicked()
                {
                    self.open_import(state);
                }
            });
        });
    }

    /// The tile menu's lifecycle items: Duplicate, Replace source…, the
    /// Category and Colour label submenus, Pause or Resume, Close or Reopen.
    #[allow(clippy::too_many_lines)] // one menu section
    pub(crate) fn draw_lifecycle_menu(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        tile: LifecycleMenu<'_>,
        intents: &mut Vec<StudioIntent>,
    ) {
        let Some(view) = state.view.as_ref() else {
            return;
        };
        let l = self.locale;
        let LifecycleMenu {
            input,
            name,
            edit: (editable, edit_reason),
            mark,
        } = tile;
        let wire = WireInputId::from_domain(input);
        let status = view.lifecycle(input);
        let push = |intents: &mut Vec<StudioIntent>, operation| {
            intents.push(StudioIntent::InputLifecycle(operation));
        };
        let duplicate = self.lifecycle_add_gate(state);
        if menu_item(
            ui,
            duplicate.enabled,
            Button::new(l.text(Msg::LcDuplicate)),
            duplicate.mark.unwrap_or(""),
            &l.format(Msg::LcDuplicateHover, &[("name", Arg::Str(name))]),
            duplicate.reason,
        ) && let Some(ids) = self.allocate_lifecycle_ids(state, 1)
        {
            push(
                intents,
                InputLifecycleOperation::Duplicate {
                    source: wire,
                    input: WireInputId::from_domain(ids[0]),
                    name: None,
                },
            );
        }
        if menu_item(
            ui,
            editable,
            Button::new(l.text(Msg::LcReplaceEllipsis)),
            mark,
            l.text(Msg::LcReplaceHover),
            edit_reason,
        ) {
            self.open_replace_source(input, name);
        }
        let categories = view.input_categories.clone();
        let mut manage = false;
        ui.add_enabled_ui(editable, |ui| {
            ui.menu_button(l.text(Msg::LcCategory), |ui| {
                ui.set_min_width(200.0);
                if ui
                    .add(Button::selectable(
                        status.category.is_none(),
                        l.text(Msg::LcCategoryNone),
                    ))
                    .on_hover_text(l.text(Msg::LcCategoryNoneHover))
                    .clicked()
                    && status.category.is_some()
                {
                    push(
                        intents,
                        InputLifecycleOperation::SetCategory {
                            input: wire,
                            category: None,
                        },
                    );
                }
                for category in &categories {
                    let selected = status.category == Some(category.id);
                    if ui
                        .add(Button::selectable(selected, category.name.as_str()))
                        .on_hover_text(l.format(
                            Msg::LcCategoryOptionHover,
                            &[("category", Arg::Str(&category.name))],
                        ))
                        .clicked()
                        && !selected
                    {
                        push(
                            intents,
                            InputLifecycleOperation::SetCategory {
                                input: wire,
                                category: Some(category.id),
                            },
                        );
                    }
                }
                ui.separator();
                let reason = project_edit_reason(state);
                if ui
                    .add_enabled(
                        reason.is_none(),
                        Button::new(l.text(Msg::LcManageCategoriesEllipsis)),
                    )
                    .hint(l.text(Msg::LcManageCategoriesHover), reason.unwrap_or(""))
                    .clicked()
                {
                    manage = true;
                }
            })
            .response
            .hint(l.text(Msg::LcCategoryHover), edit_reason);
        });
        if manage {
            self.open_categories(state);
        }
        ui.add_enabled_ui(editable, |ui| {
            ui.menu_button(l.text(Msg::LcColour), |ui| {
                ui.set_min_width(180.0);
                if ui
                    .add(Button::selectable(
                        status.color.is_none(),
                        l.text(Msg::LcColourNone),
                    ))
                    .on_hover_text(l.text(Msg::LcColourNoneHover))
                    .clicked()
                    && status.color.is_some()
                {
                    push(
                        intents,
                        InputLifecycleOperation::SetColor {
                            input: wire,
                            color: None,
                        },
                    );
                }
                for label in InputColorLabel::ALL {
                    let selected = status.color == Some(label);
                    let colour = l.text(label_name(label));
                    if ui
                        .add(Button::selectable(
                            selected,
                            (
                                RichText::new("■").color(label_color(label)),
                                RichText::new(colour),
                            ),
                        ))
                        .on_hover_text(
                            l.format(Msg::LcColourOptionHover, &[("colour", Arg::Str(colour))]),
                        )
                        .clicked()
                        && !selected
                    {
                        push(
                            intents,
                            InputLifecycleOperation::SetColor {
                                input: wire,
                                color: Some(label),
                            },
                        );
                    }
                }
            })
            .response
            .hint(l.text(Msg::LcColourHover), edit_reason);
        });
        let (pause_label, pause_hover) = if status.paused {
            (Msg::LcResume, Msg::LcResumeHover)
        } else {
            (Msg::LcPause, Msg::LcPauseHover)
        };
        if menu_item(
            ui,
            editable,
            Button::new(l.text(pause_label)),
            mark,
            l.text(pause_hover),
            edit_reason,
        ) {
            push(
                intents,
                InputLifecycleOperation::SetPaused {
                    input: wire,
                    paused: !status.paused,
                },
            );
        }
        // The server refuses closing an input on Program (`input_on_air`).
        let on_program = !status.closed
            && (view.switcher.desired.program == input || view.switcher.realized.program == input);
        let (close_label, close_hover) = if status.closed {
            (Msg::LcReopen, Msg::LcReopenHover)
        } else {
            (Msg::LcClose, Msg::LcCloseHover)
        };
        if menu_item(
            ui,
            editable && !on_program,
            Button::new(l.text(close_label)),
            if on_program && editable { "PGM" } else { mark },
            l.text(close_hover),
            if editable {
                ON_PROGRAM_REASON
            } else {
                edit_reason
            },
        ) {
            push(
                intents,
                InputLifecycleOperation::SetClosed {
                    input: wire,
                    closed: !status.closed,
                },
            );
        }
    }

    /// Draws whichever lifecycle dialogs are open.
    pub(crate) fn draw_lifecycle_dialogs(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        self.draw_replace_dialog(context, state, intents);
        self.draw_import_dialog(context, state, intents);
        self.draw_categories_dialog(context, state, intents);
    }

    #[allow(clippy::too_many_lines)] // one modal dialog
    fn draw_replace_dialog(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let Some(mut draft) = self.lifecycle.replace.take() else {
            return;
        };
        let l = self.locale;
        let gate = crate::input_edit_lock_reason(state, draft.input)
            .filter(|_| project_editing_available(state))
            .or_else(|| {
                (!project_editing_available(state))
                    .then(|| unavailable_reason(state, Capability::EditProject))
            });
        let mut submit = false;
        let mut cancel = false;
        let closed = crate::menu::help_modal(
            context,
            "studio-replace-source-window",
            l.text(Msg::LcReplaceTitle),
            460.0,
            |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            l.format(Msg::LcReplaceIntro, &[("name", Arg::Str(&draft.name))]),
                        )
                        .size(crate::theme::legible(11.0))
                        .color(text_dim_color()),
                    )
                    .wrap(),
                );
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for kind in SourceKind::ALL {
                        if ui
                            .add(
                                kind_key(l.text(kind.key()), draft.kind == kind)
                                    .min_size(Vec2::new(78.0, 26.0)),
                            )
                            .on_hover_text(l.text(kind.hover()))
                            .clicked()
                        {
                            draft.kind = kind;
                        }
                    }
                });
                ui.add_space(6.0);
                match draft.kind.field_hover() {
                    Some(hover) => {
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.text)
                                .id(egui::Id::new("studio-replace-source-text"))
                                .char_limit(MAX_INPUT_SOURCE_BYTES)
                                .desired_width(ui.available_width()),
                        )
                        .on_hover_text(l.text(hover));
                    }
                    None if draft.kind == SourceKind::Solid => {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(5.0, 5.0);
                            for (label, color) in COLOUR_PRESETS {
                                if colour_swatch(ui, color, draft.color == color)
                                    .on_hover_text(l.format(
                                        Msg::ColourSwatchHover,
                                        &[
                                            ("colour", Arg::Str(l.text(label))),
                                            ("hex", Arg::Str(&hex_colour(color))),
                                        ],
                                    ))
                                    .clicked()
                                {
                                    draft.color = color;
                                }
                            }
                            egui::color_picker::color_edit_button_srgba(
                                ui,
                                &mut draft.color,
                                egui::color_picker::Alpha::Opaque,
                            )
                            .on_hover_text(l.text(Msg::ColourPickHover));
                        });
                    }
                    None => {}
                }
                let problem = draft.source().err().or(gate);
                ui.add_space(8.0);
                ui.separator();
                dialog_keys(
                    ui,
                    problem,
                    (Msg::LcReplaceKey, Msg::LcReplaceKeyHover),
                    Msg::LcReplaceCancelHover,
                    (&mut submit, &mut cancel),
                );
            },
        );
        if submit
            && gate.is_none()
            && let Ok(source) = draft.source()
        {
            intents.push(StudioIntent::InputLifecycle(
                InputLifecycleOperation::ReplaceSource {
                    input: WireInputId::from_domain(draft.input),
                    source,
                },
            ));
            return;
        }
        if !(closed || cancel) {
            self.lifecycle.replace = Some(draft);
        }
    }

    fn draw_import_dialog(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let Some(mut draft) = self.lifecycle.import.take() else {
            return;
        };
        let l = self.locale;
        let gate = self.lifecycle_add_gate(state);
        let inputs = state.view.as_ref().map_or(0, |view| view.inputs.len())
            + self.catalogue.adds_in_flight(state);
        let mut submit = false;
        let mut cancel = false;
        let closed = crate::menu::help_modal(
            context,
            "studio-import-files-window",
            l.text(Msg::LcImportTitle),
            520.0,
            |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(l.text(Msg::LcImportIntro))
                            .size(crate::theme::legible(11.0))
                            .color(text_dim_color()),
                    )
                    .wrap(),
                );
                ui.add_space(8.0);
                let edit = ui
                    .add(
                        egui::TextEdit::multiline(&mut draft.text)
                            .id(egui::Id::new("studio-import-files-paths"))
                            .font(egui::TextStyle::Monospace)
                            .hint_text("/media/clips/opener.mov")
                            .desired_rows(8)
                            .desired_width(ui.available_width()),
                    )
                    .on_hover_text(l.text(Msg::LcImportPathsHover));
                if draft.focus_pending {
                    draft.focus_pending = false;
                    edit.request_focus();
                }
                let paths = parse_import_paths(&draft.text);
                ui.label(
                    mono(l.format(Msg::LcImportCount, &[("count", Arg::from(paths.len()))]))
                        .size(crate::theme::legible(10.5))
                        .color(muted_color()),
                );
                let problem = (!gate.enabled)
                    .then_some(gate.reason)
                    .or_else(|| import_problem(&paths, inputs));
                ui.add_space(6.0);
                ui.separator();
                dialog_keys(
                    ui,
                    problem,
                    (Msg::LcImportKey, Msg::LcImportKeyHover),
                    Msg::LcImportCancelHover,
                    (&mut submit, &mut cancel),
                );
            },
        );
        let paths = parse_import_paths(&draft.text);
        if submit
            && gate.enabled
            && import_problem(&paths, inputs).is_none()
            && let Some(ids) = self.allocate_lifecycle_ids(state, paths.len())
        {
            let items = ids
                .into_iter()
                .zip(paths)
                .map(|(input, uri)| ImportItemSpec {
                    input: WireInputId::from_domain(input),
                    name: None,
                    uri,
                })
                .collect();
            intents.push(StudioIntent::InputLifecycle(
                InputLifecycleOperation::Import { items },
            ));
            return;
        }
        if !(closed || cancel) {
            self.lifecycle.import = Some(draft);
        }
    }

    #[allow(clippy::too_many_lines)] // one modal dialog
    fn draw_categories_dialog(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let Some(mut draft) = self.lifecycle.categories.take() else {
            return;
        };
        let Some(view) = state.view.as_ref() else {
            return;
        };
        let l = self.locale;
        let gate = project_edit_reason(state);
        let mut submit = false;
        let mut cancel = false;
        let closed = crate::menu::help_modal(
            context,
            "studio-input-categories-window",
            l.text(Msg::LcCategoriesTitle),
            440.0,
            |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(l.text(Msg::LcCategoriesIntro))
                            .size(crate::theme::legible(11.0))
                            .color(text_dim_color()),
                    )
                    .wrap(),
                );
                ui.add_space(8.0);
                if draft.rows.is_empty() {
                    ui.label(
                        RichText::new(l.text(Msg::LcCategoriesEmpty))
                            .size(crate::theme::legible(10.5))
                            .color(muted_color()),
                    );
                }
                let mut remove = None;
                for (index, row) in draft.rows.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut row.name)
                                .id(egui::Id::new(("studio-category-name", row.id)))
                                .char_limit(MAX_INPUT_CATEGORY_NAME_BYTES)
                                .desired_width(ui.available_width() - 90.0),
                        )
                        .on_hover_text(l.text(Msg::LcCategoryNameHover));
                        let in_use = category_in_use(view, row.id);
                        if ui
                            .add_enabled(!in_use, small_button(l.text(Msg::LcCategoryRemove)))
                            .hint(
                                l.text(Msg::LcCategoryRemoveHover),
                                Msg::LcReasonCategoryInUse.en(),
                            )
                            .clicked()
                        {
                            remove = Some(index);
                        }
                    });
                }
                if let Some(index) = remove {
                    draft.rows.remove(index);
                }
                ui.add_space(4.0);
                ui.label(section_label(l.text(Msg::LcCategoryAdd)));
                let add_problem = if draft.rows.len() >= MAX_INPUT_CATEGORIES {
                    Some(Msg::LcReasonCategoryLimit.en())
                } else {
                    category_name_problem(
                        &draft.new_name,
                        draft.rows.iter().map(|row| row.name.as_str()),
                    )
                };
                ui.horizontal(|ui| {
                    let edit = ui
                        .add(
                            egui::TextEdit::singleline(&mut draft.new_name)
                                .id(egui::Id::new("studio-category-new"))
                                .char_limit(MAX_INPUT_CATEGORY_NAME_BYTES)
                                .desired_width(ui.available_width() - 90.0),
                        )
                        .on_hover_text(l.text(Msg::LcCategoryNewHover));
                    let entered =
                        edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    let add = ui
                        .add_enabled(
                            add_problem.is_none(),
                            small_button(l.text(Msg::LcCategoryAdd)),
                        )
                        .hint(l.text(Msg::LcCategoryAddHover), add_problem.unwrap_or(""));
                    if (add.clicked() || entered)
                        && add_problem.is_none()
                        && let Some(id) =
                            next_category_id(draft.rows.iter().chain(&view.input_categories))
                    {
                        draft.rows.push(InputCategoryStatus {
                            id,
                            name: draft.new_name.trim().to_owned(),
                        });
                        draft.new_name.clear();
                    }
                });
                let problem =
                    gate.or_else(|| categories_problem(&draft.rows, &view.input_categories));
                ui.add_space(8.0);
                ui.separator();
                dialog_keys(
                    ui,
                    problem,
                    (Msg::ApplyKey, Msg::LcCategoriesApplyHover),
                    Msg::LcCategoriesCancelHover,
                    (&mut submit, &mut cancel),
                );
            },
        );
        if submit
            && gate.is_none()
            && categories_problem(&draft.rows, &view.input_categories).is_none()
        {
            intents.push(StudioIntent::InputLifecycle(
                InputLifecycleOperation::SetCategories {
                    categories: trimmed_categories(&draft.rows),
                },
            ));
            return;
        }
        if !(closed || cancel) {
            self.lifecycle.categories = Some(draft);
        }
    }
}

/// A dialog's footer: the blocking reason (if any) and the Cancel and
/// confirm keys, confirm on the trailing side but reached by Tab last.
fn dialog_keys(
    ui: &mut Ui,
    problem: Option<&'static str>,
    (confirm, confirm_hover): (Msg, Msg),
    cancel_hover: Msg,
    (submit, cancel): (&mut bool, &mut bool),
) {
    let l = l10n::locale(ui.ctx());
    ui.horizontal(|ui| {
        if let Some(reason) = problem {
            let (lamp, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
            paint_lamp(ui.painter(), lamp.center(), 4.0, Tone::Pending);
            ui.add(
                egui::Label::new(
                    RichText::new(capitalized(l.tr(reason)))
                        .size(crate::theme::legible(10.5))
                        .color(AMBER),
                )
                .truncate(),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let slot = ui
                .allocate_exact_size(Vec2::new(110.0, 26.0), Sense::hover())
                .0;
            if ui
                .add(
                    Button::new(
                        RichText::new(l.text(Msg::CancelKey))
                            .size(12.0)
                            .color(text_dim_color()),
                    )
                    .min_size(Vec2::new(84.0, 26.0)),
                )
                .on_hover_text(with_key(l, l.text(cancel_hover), "Esc"))
                .clicked()
            {
                *cancel = true;
            }
            if ui
                .add_enabled_ui(problem.is_none(), |ui| {
                    ui.put(
                        slot,
                        Button::new(
                            RichText::new(l.text(confirm))
                                .size(12.0)
                                .strong()
                                .color(text_color()),
                        )
                        .fill(ACCENT_FILL)
                        .stroke(Stroke::new(1.0, accent_color()))
                        .min_size(Vec2::new(110.0, 26.0)),
                    )
                })
                .inner
                .hint(l.text(confirm_hover), problem.unwrap_or(""))
                .clicked()
            {
                *submit = true;
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    fn id(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn category(id: u32, name: &str) -> InputCategoryStatus {
        InputCategoryStatus {
            id,
            name: name.to_owned(),
        }
    }

    fn filed(category: Option<u32>) -> InputLifecycleStatus {
        InputLifecycleStatus {
            category,
            ..InputLifecycleStatus::default()
        }
    }

    #[test]
    fn the_filter_shows_all_one_category_or_the_uncategorised() {
        assert!(BankFilter::All.shows(filed(Some(2))));
        assert!(BankFilter::All.shows(filed(None)));
        assert!(BankFilter::Category(2).shows(filed(Some(2))));
        assert!(!BankFilter::Category(2).shows(filed(Some(3))));
        assert!(!BankFilter::Category(2).shows(filed(None)));
        assert!(BankFilter::Uncategorised.shows(filed(None)));
        assert!(!BankFilter::Uncategorised.shows(filed(Some(1))));
    }

    #[test]
    fn a_filter_on_a_vanished_category_falls_back_to_all() {
        let categories = [category(1, "Cameras")];
        assert_eq!(
            BankFilter::Category(1).reconciled(&categories),
            BankFilter::Category(1)
        );
        assert_eq!(
            BankFilter::Category(2).reconciled(&categories),
            BankFilter::All
        );
        assert_eq!(
            BankFilter::Uncategorised.reconciled(&categories),
            BankFilter::Uncategorised
        );
        assert_eq!(BankFilter::Uncategorised.reconciled(&[]), BankFilter::All);
    }

    #[test]
    fn every_label_has_a_distinct_opaque_colour_and_a_name() {
        let colours: Vec<Color32> = InputColorLabel::ALL.map(label_color).to_vec();
        for (index, colour) in colours.iter().enumerate() {
            assert_eq!(colour.a(), 255);
            assert!(!colours[index + 1..].contains(colour));
        }
        assert_eq!(
            label_color(InputColorLabel::Blue),
            Color32::from_rgb(0, 0x90, 0xFF)
        );
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(spanish.text(label_name(InputColorLabel::Teal)), "Turquesa");
        assert_eq!(
            Locale::ENGLISH.text(label_name(InputColorLabel::Pink)),
            "Pink"
        );
    }

    #[test]
    fn badges_put_offline_first_then_closed_then_paused() {
        assert_eq!(lifecycle_badges(InputLifecycleStatus::default()), []);
        let status = InputLifecycleStatus {
            paused: true,
            closed: true,
            offline: Some(InputOfflineReason::SourceMissing),
            ..InputLifecycleStatus::default()
        };
        assert_eq!(
            lifecycle_badges(status),
            [
                LifecycleBadge::Offline(InputOfflineReason::SourceMissing),
                LifecycleBadge::Closed,
                LifecycleBadge::Paused,
            ]
        );
        let line = tile_status_line(
            Locale::ENGLISH,
            "Clip",
            InputLifecycleStatus {
                category: Some(4),
                ..status
            },
            &[category(4, "Playback")],
        );
        assert_eq!(
            line,
            "Clip · Playback · SOURCE OFFLINE (source_missing) · CLOSED · PAUSED"
        );
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(
            LifecycleBadge::Offline(InputOfflineReason::DeviceUnavailable).detail(spanish),
            "SIN SEÑAL (device_unavailable)"
        );
        // A category id the list lacks is left out rather than guessed.
        assert_eq!(
            tile_status_line(Locale::ENGLISH, "Cam", filed(Some(9)), &[]),
            "Cam"
        );
    }

    #[test]
    fn pasted_paths_are_trimmed_unquoted_and_deduplicated() {
        let text = "  /media/a.mov \n\n\"/media/b c.mp4\"\n'/media/d.wav'\n/media/a.mov\r\n  \n";
        assert_eq!(
            parse_import_paths(text),
            ["/media/a.mov", "/media/b c.mp4", "/media/d.wav"]
        );
        assert_eq!(parse_import_paths("\n  \n"), Vec::<String>::new());
    }

    #[test]
    fn imports_are_checked_against_the_batch_and_input_limits() {
        let paths = |count: usize| -> Vec<String> {
            (0..count).map(|index| format!("/m/{index}.mov")).collect()
        };
        assert_eq!(import_problem(&[], 0), Some(Msg::LcReasonNoPaths.en()));
        assert_eq!(import_problem(&paths(3), 10), None);
        assert_eq!(
            import_problem(&paths(MAX_BATCH_IMPORT_ITEMS + 1), 0),
            Some(Msg::LcReasonTooManyItems.en())
        );
        assert_eq!(
            import_problem(&paths(2), MAX_LIVE_INPUTS - 1),
            Some(Msg::LcReasonImportOverLimit.en())
        );
        assert_eq!(
            import_problem(&["x".repeat(MAX_INPUT_SOURCE_BYTES + 1)], 0),
            Some(Msg::LcReasonPathTooLong.en())
        );
    }

    #[test]
    fn allocated_ids_follow_the_largest_taken_and_never_repeat() {
        assert_eq!(
            allocate_input_ids(&[id(3), id(7)], 3),
            Some(vec![id(8), id(9), id(10)])
        );
        assert_eq!(allocate_input_ids(&[], 1), Some(vec![id(1)]));
        assert_eq!(allocate_input_ids(&[id(2)], 0), Some(Vec::new()));
    }

    #[test]
    fn category_names_are_unique_case_insensitively_and_bounded() {
        let others = ["Cameras", "Graphics"];
        assert_eq!(category_name_problem("Playback", others), None);
        assert_eq!(
            category_name_problem(" cameras ", others),
            Some(Msg::LcReasonCategoryTaken.en())
        );
        assert_eq!(
            category_name_problem("  ", others),
            Some(Msg::ReasonNameEmpty.en())
        );
        assert_eq!(
            category_name_problem(&"x".repeat(MAX_INPUT_CATEGORY_NAME_BYTES + 1), others),
            Some(Msg::LcReasonCategoryNameTooLong.en())
        );
        assert_eq!(
            next_category_id(&[category(2, "A"), category(5, "B")]),
            Some(6)
        );
        assert_eq!(next_category_id(&[]), Some(1));
    }

    #[test]
    fn category_drafts_must_change_and_stay_valid() {
        let current = [category(1, "Cameras")];
        assert_eq!(
            categories_problem(&current, &current),
            Some(Msg::LcReasonCategoriesUnchanged.en())
        );
        // Whitespace alone is not a change.
        assert_eq!(
            categories_problem(&[category(1, " Cameras ")], &current),
            Some(Msg::LcReasonCategoriesUnchanged.en())
        );
        assert_eq!(categories_problem(&[category(1, "Cams")], &current), None);
        assert_eq!(categories_problem(&[], &current), None);
        assert_eq!(
            categories_problem(&[category(1, "A"), category(2, "a")], &current),
            Some(Msg::LcReasonCategoryTaken.en())
        );
        let many: Vec<InputCategoryStatus> = (1..=u32::try_from(MAX_INPUT_CATEGORIES).unwrap() + 1)
            .map(|id| category(id, &format!("C{id}")))
            .collect();
        assert_eq!(
            categories_problem(&many, &current),
            Some(Msg::LcReasonCategoryLimit.en())
        );
    }

    #[test]
    fn replacement_sources_follow_the_chosen_kind() {
        let mut draft = ReplaceDraft::new(id(1), "Camera 1");
        draft.color = Color32::from_rgb(1, 2, 3);
        assert_eq!(
            draft.source(),
            Ok(WireInputSource::Generator {
                video: WireGeneratorVideo::Solid {
                    rgba: [1, 2, 3, 255]
                },
                tone_hz: None,
            })
        );
        draft.kind = SourceKind::Bars;
        assert_eq!(
            draft.source(),
            Ok(WireInputSource::Generator {
                video: WireGeneratorVideo::Bars,
                tone_hz: None,
            })
        );
        draft.kind = SourceKind::Media;
        assert_eq!(draft.source(), Err(Msg::LcReasonSourceEmpty.en()));
        draft.text = "  /media/clip.mov ".into();
        assert_eq!(
            draft.source(),
            Ok(WireInputSource::Media {
                uri: "/media/clip.mov".into()
            })
        );
        draft.kind = SourceKind::Device;
        assert_eq!(
            draft.source(),
            Ok(WireInputSource::Device {
                stable_key: "/media/clip.mov".into()
            })
        );
        draft.kind = SourceKind::Network;
        draft.text = "x".repeat(MAX_INPUT_SOURCE_BYTES + 1);
        assert_eq!(draft.source(), Err(Msg::LcReasonSourceTooLong.en()));
    }
}
