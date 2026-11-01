//! Still image and slideshow inputs in the console (IN-008, IN-009).
//!
//! The Add input dialog's Image form takes one picture (an absolute path on
//! the server's machine, which the server copies into the project, or an
//! `asset://` URI already in it) and how it is placed (Fit, Fill, Stretch,
//! or 1:1). The Slideshow form takes a folder (copied into the project,
//! whose pictures the server then follows live: pictures added to or
//! removed from the project's copy join or leave the show) or a list of
//! pictures (one per line, `path | caption` for a caption), the time each
//! slide holds, a cut or a fade, Ken Burns, random order, looping, and
//! auto-advance. A file or folder dropped on the window fills the form.
//!
//! The Media tab shows a panel per slideshow input: First, Previous, Next,
//! and Last keys, the slide on air (`3 / 12`) and its name or caption, and
//! a strip of every slide by number and name, where a click shows that
//! slide. The strip asks the server for the slide list window by window.

use egui::{RichText, Ui};
use fm_protocol::{
    ImageInputConfig, PagedNavigation, PagedPageKey, PagedPageRequest, SlideOrder,
    SlideTransitionStyle, SlideshowConfig, SlideshowItem, SlideshowSource, WireInputId,
    WireStillSource,
};
use fm_types::{FrameRate, InputId, OutputScaling};
use fm_ui_model::{ClientView, StillInputStatus};

use crate::StudioIntent;
use crate::StudioUiState;
use crate::gating::Hint;
use crate::input_catalogue::kind_key;
use crate::l10n::{Arg, Locale, Msg};
use crate::network_stream_ui::{FieldProblem, field_error, hint_slot, mark_invalid};
use crate::theme::{KeyIcon, add_icon_button, muted_color, section_label, text_color};

/// Every fit, with its key label and its hover help.
const FITS: [(OutputScaling, Msg, Msg); 4] = [
    (OutputScaling::Fit, Msg::StFitFit, Msg::StFitFitHover),
    (OutputScaling::Fill, Msg::StFitFill, Msg::StFitFillHover),
    (
        OutputScaling::Stretch,
        Msg::StFitStretch,
        Msg::StFitStretchHover,
    ),
    (
        OutputScaling::OneToOne,
        Msg::StFitOneToOne,
        Msg::StFitOneToOneHover,
    ),
];

/// Longest slide hold the form offers, in seconds.
const MAX_SLIDE_SECONDS: f64 = 3_600.0;
/// Longest fade the form offers, in seconds.
const MAX_FADE_SECONDS: f64 = 60.0;

/// The field of the Image or Slideshow form a problem belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StillField {
    Picture,
    Folder,
    Pictures,
}

impl StillField {
    pub(crate) const fn label(self) -> Msg {
        match self {
            Self::Picture => Msg::StPicture,
            Self::Folder => Msg::StFolder,
            Self::Pictures => Msg::StPictures,
        }
    }
}

/// A typed path: trimmed, without surrounding quotes, and a `file://` URL
/// as its path.
fn clean_path(text: &str) -> String {
    let mut path = text.trim();
    for quote in ['"', '\''] {
        if path.len() >= 2 && path.starts_with(quote) && path.ends_with(quote) {
            path = path[1..path.len() - 1].trim();
        }
    }
    path.strip_prefix("file://").unwrap_or(path).to_owned()
}

/// The Image form of the Add input dialog.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ImageDraft {
    pub(crate) path: String,
    pub(crate) fit: OutputScaling,
}

impl ImageDraft {
    /// The configuration the server would accept, or the field to fix and
    /// why.
    pub(crate) fn checked(&self) -> Result<ImageInputConfig, FieldProblem<StillField>> {
        let path = clean_path(&self.path);
        if path.is_empty() {
            return Err(FieldProblem::prompt(
                StillField::Picture,
                Msg::StReasonPictureEmpty.en(),
            ));
        }
        if !fm_types::still::is_image_file_name(&path) {
            return Err(FieldProblem::error(
                StillField::Picture,
                Msg::StReasonPictureType.en(),
            ));
        }
        let config = ImageInputConfig::new(path, self.fit);
        config
            .validate_request()
            .map_err(|_| FieldProblem::error(StillField::Picture, Msg::StReasonPathInvalid.en()))?;
        Ok(config)
    }

    /// The configuration, or why not (an English catalog reason).
    pub(crate) fn config(&self) -> Result<ImageInputConfig, &'static str> {
        self.checked().map_err(|problem| problem.reason)
    }
}

/// The Slideshow form of the Add input dialog.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct SlideshowDraft {
    /// A folder; otherwise a list of pictures.
    pub(crate) folder_mode: bool,
    pub(crate) folder: String,
    /// One picture per line, `path | caption` for a caption.
    pub(crate) pictures: String,
    pub(crate) slide_seconds: f64,
    pub(crate) transition: SlideTransitionStyle,
    pub(crate) fade_seconds: f64,
    pub(crate) ken_burns: bool,
    pub(crate) random: bool,
    pub(crate) loop_show: bool,
    pub(crate) auto_advance: bool,
    pub(crate) fit: OutputScaling,
    /// The show's frame rate, which turns seconds into frames.
    pub(crate) rate: FrameRate,
    /// The random order's seed, fixed when the form opened.
    pub(crate) seed: u64,
}

// The seconds fields only ever hold finite numbers (the form's ranges), so
// equality is reflexive.
impl Eq for SlideshowDraft {}

impl SlideshowDraft {
    pub(crate) fn new(rate: FrameRate, seed: u64) -> Self {
        Self {
            folder_mode: true,
            folder: String::new(),
            pictures: String::new(),
            slide_seconds: 5.0,
            transition: SlideTransitionStyle::Fade,
            fade_seconds: 1.0,
            ken_burns: false,
            random: false,
            loop_show: true,
            auto_advance: true,
            fit: OutputScaling::Fit,
            rate,
            seed,
        }
    }

    /// Whole frames of `seconds` at the show's rate, at least one.
    fn frames(&self, seconds: f64) -> u32 {
        let fps = f64::from(self.rate.numerator()) / f64::from(self.rate.denominator().max(1));
        let frames = (seconds * fps).round();
        if frames < 1.0 {
            1
        } else if frames >= f64::from(u32::MAX) {
            u32::MAX
        } else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let frames = frames as u32;
            frames
        }
    }

    /// The pictures of the list, each with its caption.
    fn items(&self) -> Vec<SlideshowItem> {
        self.pictures
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| match line.split_once(" | ") {
                Some((path, caption)) if !caption.trim().is_empty() => {
                    SlideshowItem::new(clean_path(path)).with_caption(caption.trim())
                }
                Some((path, _)) => SlideshowItem::new(clean_path(path)),
                None => SlideshowItem::new(clean_path(line)),
            })
            .collect()
    }

    /// The configuration the server would accept, or the field to fix and
    /// why.
    pub(crate) fn checked(&self) -> Result<SlideshowConfig, FieldProblem<StillField>> {
        let source = if self.folder_mode {
            let folder = clean_path(&self.folder);
            let folder = folder.trim_end_matches(['/', '\\']).to_owned();
            if folder.is_empty() {
                return Err(FieldProblem::prompt(
                    StillField::Folder,
                    Msg::StReasonFolderEmpty.en(),
                ));
            }
            if !fm_types::still::request_uri_is_valid(
                &folder,
                fm_types::still::MAX_SLIDESHOW_FOLDER_BYTES,
            ) {
                return Err(FieldProblem::error(
                    StillField::Folder,
                    Msg::StReasonPathInvalid.en(),
                ));
            }
            SlideshowSource::Folder { asset_uri: folder }
        } else {
            let items = self.items();
            if items.is_empty() {
                return Err(FieldProblem::prompt(
                    StillField::Pictures,
                    Msg::StReasonPicturesEmpty.en(),
                ));
            }
            if items.len() > fm_types::still::MAX_SLIDESHOW_ITEMS {
                return Err(FieldProblem::error(
                    StillField::Pictures,
                    Msg::StReasonPicturesTooMany.en(),
                ));
            }
            if items
                .iter()
                .any(|item| !fm_types::still::is_image_file_name(&item.asset_uri))
            {
                return Err(FieldProblem::error(
                    StillField::Pictures,
                    Msg::StReasonPictureType.en(),
                ));
            }
            SlideshowSource::List(items)
        };
        let config = SlideshowConfig {
            source,
            slide_frames: self
                .frames(self.slide_seconds)
                .min(fm_types::still::MAX_SLIDE_FRAMES),
            transition: self.transition,
            transition_frames: self
                .frames(self.fade_seconds)
                .min(fm_types::still::MAX_SLIDE_TRANSITION_FRAMES),
            ken_burns: self.ken_burns,
            order: if self.random {
                SlideOrder::Random { seed: self.seed }
            } else {
                SlideOrder::Ordered
            },
            loop_show: self.loop_show,
            auto_advance: self.auto_advance,
            fit: self.fit,
        };
        config.validate_request().map_err(|_| {
            FieldProblem::error(
                if self.folder_mode {
                    StillField::Folder
                } else {
                    StillField::Pictures
                },
                Msg::StReasonPathInvalid.en(),
            )
        })?;
        Ok(config)
    }

    /// The configuration, or why not (an English catalog reason).
    pub(crate) fn config(&self) -> Result<SlideshowConfig, &'static str> {
        self.checked().map_err(|problem| problem.reason)
    }
}

/// The paths of the files and folders dropped on the window this frame.
fn dropped_paths(ui: &Ui) -> Vec<std::path::PathBuf> {
    ui.ctx().input(|input| {
        input
            .raw
            .dropped_files
            .iter()
            .filter_map(|file| file.path.clone())
            .collect()
    })
}

/// The Fit / Fill / Stretch / 1:1 keys.
fn draw_fit(ui: &mut Ui, l: Locale, fit: &mut OutputScaling) {
    crate::theme::form_section(ui, l.text(Msg::StFit));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::Vec2::new(6.0, 6.0);
        for (choice, label, hover) in FITS {
            let selected = *fit == choice;
            if ui
                .add(kind_key(l.text(label), selected))
                .on_hover_text(l.text(hover))
                .clicked()
            {
                *fit = choice;
            }
        }
    });
}

/// One path field with its hint slot; `problem` marks it.
fn path_field(
    ui: &mut Ui,
    l: Locale,
    (id, hint, hover): (&str, &str, Msg),
    text: &mut String,
    problem: Option<FieldProblem<StillField>>,
) {
    let edit = ui
        .add(
            egui::TextEdit::singleline(text)
                .id(egui::Id::new(id))
                .hint_text(hint)
                .char_limit(fm_types::still::MAX_IMAGE_URI_BYTES)
                .desired_width(ui.available_width()),
        )
        .on_hover_text(l.text(hover));
    if problem.is_some_and(|problem| !problem.prompt) {
        mark_invalid(ui, edit.rect);
    }
    hint_slot(
        ui,
        Some(match problem {
            Some(problem) if !problem.prompt => field_error(l, problem.reason),
            _ => crate::theme::caption(l.text(Msg::StPathCaption)),
        }),
    );
}

/// Draws the Image form; a picture file dropped on the window fills it.
pub(crate) fn draw_image_form(ui: &mut Ui, l: Locale, draft: &mut ImageDraft) {
    if let Some(path) = dropped_paths(ui).into_iter().find(|path| path.is_file()) {
        draft.path = path.to_string_lossy().into_owned();
    }
    ui.label(section_label(l.text(Msg::StPicture)));
    let problem = draft.checked().err();
    path_field(
        ui,
        l,
        (
            "studio-add-input-image-path",
            "/photos/logo.png",
            Msg::StPictureHover,
        ),
        &mut draft.path,
        problem,
    );
    draw_fit(ui, l, &mut draft.fit);
}

/// Draws the Slideshow form; a folder dropped on the window becomes its
/// folder, and dropped picture files join its list.
#[allow(clippy::too_many_lines)] // one form
pub(crate) fn draw_slideshow_form(ui: &mut Ui, l: Locale, draft: &mut SlideshowDraft) {
    for path in dropped_paths(ui) {
        if path.is_dir() {
            draft.folder_mode = true;
            draft.folder = path.to_string_lossy().into_owned();
        } else {
            draft.folder_mode = false;
            if !draft.pictures.is_empty() && !draft.pictures.ends_with('\n') {
                draft.pictures.push('\n');
            }
            draft.pictures.push_str(&path.to_string_lossy());
        }
    }
    ui.label(section_label(l.text(Msg::StSource)));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (folder, label, hover) in [
            (true, Msg::StSourceFolder, Msg::StSourceFolderHover),
            (false, Msg::StSourceList, Msg::StSourceListHover),
        ] {
            let selected = draft.folder_mode == folder;
            if ui
                .add(kind_key(l.text(label), selected))
                .on_hover_text(l.text(hover))
                .clicked()
            {
                draft.folder_mode = folder;
            }
        }
    });
    let problem = draft.checked().err();
    if draft.folder_mode {
        crate::theme::form_section(ui, l.text(Msg::StFolder));
        path_field(
            ui,
            l,
            (
                "studio-add-input-slideshow-folder",
                "/photos/event",
                Msg::StFolderHover,
            ),
            &mut draft.folder,
            problem.filter(|problem| problem.field == StillField::Folder),
        );
    } else {
        crate::theme::form_section(ui, l.text(Msg::StPictures));
        let edit = ui
            .add(
                egui::TextEdit::multiline(&mut draft.pictures)
                    .id(egui::Id::new("studio-add-input-slideshow-pictures"))
                    .font(egui::TextStyle::Monospace)
                    .hint_text("/photos/one.jpg\n/photos/two.png | Caption")
                    .desired_rows(4)
                    .desired_width(ui.available_width()),
            )
            .on_hover_text(l.text(Msg::StPicturesHover));
        let problem = problem.filter(|problem| problem.field == StillField::Pictures);
        if problem.is_some_and(|problem| !problem.prompt) {
            mark_invalid(ui, edit.rect);
        }
        hint_slot(
            ui,
            Some(match problem {
                Some(problem) if !problem.prompt => field_error(l, problem.reason),
                _ => crate::theme::caption(l.text(Msg::StPicturesCaption)),
            }),
        );
    }
    crate::theme::form_section(ui, l.text(Msg::StTiming));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::Vec2::new(6.0, 6.0);
        ui.label(RichText::new(l.text(Msg::StSlideSeconds)).color(text_color()));
        ui.add(
            egui::DragValue::new(&mut draft.slide_seconds)
                .range(0.1..=MAX_SLIDE_SECONDS)
                .speed(0.1)
                .max_decimals(1)
                .suffix(" s"),
        )
        .on_hover_text(l.text(Msg::StSlideSecondsHover));
    });
    // The transition keys on their own row, the length beside them, so the
    // length never wraps away from the key it belongs to.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::Vec2::new(6.0, 6.0);
        for (transition, label, hover) in [
            (SlideTransitionStyle::Cut, Msg::StCut, Msg::StCutHover),
            (SlideTransitionStyle::Fade, Msg::StFade, Msg::StFadeHover),
            (SlideTransitionStyle::Wipe, Msg::StWipe, Msg::StWipeHover),
            (SlideTransitionStyle::Slide, Msg::StSlide, Msg::StSlideHover),
        ] {
            let selected = draft.transition == transition;
            if ui
                .add(kind_key(l.text(label), selected))
                .on_hover_text(l.text(hover))
                .clicked()
            {
                draft.transition = transition;
            }
        }
        if draft.transition != SlideTransitionStyle::Cut {
            ui.add(
                egui::DragValue::new(&mut draft.fade_seconds)
                    .range(0.1..=MAX_FADE_SECONDS)
                    .speed(0.05)
                    .max_decimals(2)
                    .suffix(" s"),
            )
            .on_hover_text(l.text(Msg::StFadeSecondsHover));
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::Vec2::new(10.0, 6.0);
        ui.checkbox(&mut draft.ken_burns, l.text(Msg::StKenBurns))
            .on_hover_text(l.text(Msg::StKenBurnsHover));
        ui.checkbox(&mut draft.random, l.text(Msg::StRandom))
            .on_hover_text(l.text(Msg::StRandomHover));
        ui.checkbox(&mut draft.loop_show, l.text(Msg::StLoop))
            .on_hover_text(l.text(Msg::StLoopHover));
        ui.checkbox(&mut draft.auto_advance, l.text(Msg::StAutoAdvance))
            .on_hover_text(l.text(Msg::StAutoAdvanceHover));
    });
    draw_fit(ui, l, &mut draft.fit);
}

/// Every slideshow input of the show, in show order, with its status.
pub(crate) fn slideshows(view: &ClientView) -> Vec<(InputId, &StillInputStatus)> {
    view.inputs
        .iter()
        .filter_map(|input| {
            let still = view.still_input(*input)?;
            matches!(still.source, WireStillSource::Slideshow(_)).then_some((*input, still))
        })
        .collect()
}

/// The slide-list window a slideshow panel still needs, if any: the first
/// slide not fetched, or the first window before any arrived.
pub(crate) fn missing_window(still: &StillInputStatus) -> Option<u32> {
    match &still.listing {
        None => (still.status.page.count > 0).then_some(0),
        Some(listing) => listing.next_missing(),
    }
}

/// Thumbnails in a slide strip: the slide on air and up to this many on
/// each side.
pub const SLIDE_THUMBNAIL_REACH: u32 = 4;

/// The slide thumbnail requests a strip around the on-air slide still
/// needs: slides with no answer yet, not already asked at this listing
/// generation. Marks them asked.
#[must_use]
pub(crate) fn wanted_slide_thumbnails(
    state: &StudioUiState,
    input: InputId,
    still: &StillInputStatus,
    asked: &mut std::collections::BTreeSet<(PagedPageKey, u64)>,
) -> Vec<PagedPageRequest> {
    if still.listing.is_none() || still.status.page.count == 0 {
        return Vec::new();
    }
    let generation = still.status.generation;
    let page = still.status.page.index;
    let count = still.status.page.count;
    let first = page.saturating_sub(SLIDE_THUMBNAIL_REACH);
    let end = page
        .saturating_add(SLIDE_THUMBNAIL_REACH)
        .saturating_add(1)
        .min(count);
    let input = WireInputId::from_domain(input);
    (first..end)
        .filter_map(|slide| {
            let key = PagedPageKey {
                input,
                page: slide,
                thumbnail: true,
            };
            if state.paged_pages.contains_key(&key) || !asked.insert((key, generation)) {
                return None;
            }
            Some(PagedPageRequest {
                request_id: format!("studio-slide-{input}-{slide}-{generation}"),
                input,
                page: slide,
                thumbnail: true,
            })
        })
        .collect()
}

/// `3 / 12`, or `0 / 0` with nothing listed.
pub(crate) fn page_text(still: &StillInputStatus) -> String {
    let page = still.status.page;
    if page.count == 0 {
        "0 / 0".to_owned()
    } else {
        format!("{} / {}", page.index + 1, page.count)
    }
}

/// One slideshow's panel: the step keys, the slide on air, and the strip.
/// `block` is why stepping is unavailable, if it is.
#[allow(clippy::too_many_lines)]
pub(crate) fn draw_slideshow_panel(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    (input, name): (InputId, &str),
    still: &StillInputStatus,
    block: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    let enabled = block.is_none() && still.status.page.count > 0;
    let reason = block.unwrap_or(Msg::StReasonNoSlides.en());
    let mut navigate = |navigation| {
        intents.push(StudioIntent::PageNavigate { input, navigation });
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new(name).strong().color(text_color()));
        ui.label(
            crate::theme::mono(page_text(still))
                .size(crate::theme::legible(11.0))
                .color(muted_color()),
        )
        .on_hover_text(l.text(Msg::StPageHover));
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let key = |label: &str| crate::playlist::key_button(label, None, 52.0);
        if ui
            .add_enabled(enabled, key(l.text(Msg::StFirst)))
            .hint(l.text(Msg::StFirstHover), reason)
            .clicked()
        {
            navigate(PagedNavigation::First);
        }
        if add_icon_button(
            ui,
            enabled,
            crate::playlist::key_button("", None, 36.0),
            KeyIcon::Back,
            l.text(Msg::StPreviousHover),
        )
        .hint(l.text(Msg::StPreviousHover), reason)
        .clicked()
        {
            navigate(PagedNavigation::Previous);
        }
        if add_icon_button(
            ui,
            enabled,
            crate::playlist::key_button("", None, 36.0),
            KeyIcon::Play,
            l.text(Msg::StNextHover),
        )
        .hint(l.text(Msg::StNextHover), reason)
        .clicked()
        {
            navigate(PagedNavigation::Next);
        }
        if ui
            .add_enabled(enabled, key(l.text(Msg::StLast)))
            .hint(l.text(Msg::StLastHover), reason)
            .clicked()
        {
            navigate(PagedNavigation::Last);
        }
        let label = if still.status.label.is_empty() {
            l.text(Msg::StNoSlide).to_owned()
        } else {
            still.status.label.clone()
        };
        ui.add(
            egui::Label::new(RichText::new(label).color(text_color()))
                .truncate()
                .sense(egui::Sense::hover()),
        )
        .on_hover_text(l.text(Msg::StSlideNameHover));
    });
    let Some(listing) = &still.listing else {
        return;
    };
    egui::ScrollArea::horizontal()
        .id_salt(("studio-slideshow-strip", input))
        .max_height(44.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (index, slide) in &listing.slides {
                    let current = *index == still.status.page.index;
                    let file = slide
                        .asset_uri
                        .rsplit('/')
                        .next()
                        .unwrap_or(&slide.asset_uri);
                    let title = slide.caption.as_deref().unwrap_or(file);
                    let text = format!("{}  {}", index + 1, title);
                    let tone = current.then_some(crate::theme::PREVIEW);
                    let key = PagedPageKey {
                        input: WireInputId::from_domain(input),
                        page: *index,
                        thumbnail: true,
                    };
                    let texture = crate::documents_ui::thumbnail_texture(
                        ui,
                        key,
                        state.paged_pages.get(&key),
                    );
                    let button = match &texture {
                        Some(texture) => egui::Button::image(
                            egui::Image::new(texture)
                                .fit_to_exact_size(egui::Vec2::new(64.0, 36.0)),
                        )
                        .min_size(egui::Vec2::new(64.0, 36.0)),
                        None => crate::playlist::key_button(&text, tone, 120.0),
                    };
                    if ui
                        .add_enabled(block.is_none(), button)
                        .hint(
                            &l.format(
                                Msg::StSlideKeyHover,
                                &[
                                    ("page", Arg::from(u64::from(*index) + 1)),
                                    ("name", Arg::Str(title)),
                                ],
                            ),
                            reason,
                        )
                        .clicked()
                    {
                        intents.push(StudioIntent::PageNavigate {
                            input,
                            navigation: PagedNavigation::Goto(*index),
                        });
                    }
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate() -> FrameRate {
        FrameRate::new(25, 1).unwrap()
    }

    #[test]
    fn an_image_needs_a_picture_path() {
        let mut draft = ImageDraft::default();
        assert!(draft.checked().unwrap_err().prompt);
        draft.path = "/photos/notes.txt".into();
        assert_eq!(draft.config(), Err(Msg::StReasonPictureType.en()));
        draft.path = "relative/logo.png".into();
        assert_eq!(draft.config(), Err(Msg::StReasonPathInvalid.en()));
        draft.path = " \"/photos/logo.png\" ".into();
        draft.fit = OutputScaling::Fill;
        assert_eq!(
            draft.config(),
            Ok(ImageInputConfig::new(
                "/photos/logo.png",
                OutputScaling::Fill
            ))
        );
        draft.path = "asset://logo.png".into();
        assert!(draft.config().is_ok());
    }

    #[test]
    fn a_slideshow_turns_seconds_into_frames() {
        let mut draft = SlideshowDraft::new(rate(), 7);
        assert!(draft.checked().unwrap_err().prompt);
        draft.folder = "/photos/event/".into();
        draft.slide_seconds = 4.0;
        draft.fade_seconds = 0.5;
        draft.random = true;
        let config = draft.config().unwrap();
        assert_eq!(
            config.source,
            SlideshowSource::Folder {
                asset_uri: "/photos/event".into()
            }
        );
        assert_eq!(config.slide_frames, 100);
        assert_eq!(config.transition_frames, 13);
        assert_eq!(config.order, SlideOrder::Random { seed: 7 });
    }

    #[test]
    fn a_slideshow_list_takes_captions() {
        let mut draft = SlideshowDraft::new(rate(), 1);
        draft.folder_mode = false;
        draft.pictures = "/a/one.jpg\n\n/a/two.png | Second\n".into();
        let config = draft.config().unwrap();
        assert_eq!(
            config.source,
            SlideshowSource::List(vec![
                SlideshowItem::new("/a/one.jpg"),
                SlideshowItem::new("/a/two.png").with_caption("Second"),
            ])
        );
        draft.pictures = "/a/one.mov".into();
        assert_eq!(draft.config(), Err(Msg::StReasonPictureType.en()));
    }
}
