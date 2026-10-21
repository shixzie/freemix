//! Input lifecycle between the wire and the engine (Protocol 2.38, IN-026).

use std::collections::BTreeMap;

use fm_engine::{EngineInputLifecycleEdit, EngineMediaFault, ShowState};
use fm_model::{
    BatchImportRefusal, BatchImportRequest, GeneratorVideo, InputCategory, InputCategoryId,
    InputSourceSpec,
};
use fm_protocol::{
    AddedInput, EventPayload, ImportItemSpec, ImportRefusal, InputCategoryStatus, InputColorLabel,
    InputLifecycleOperation, InputLifecycleStatus, InputOfflineReason, WireGeneratorVideo,
    WireInputId, WireInputSource,
};
use fm_types::InputId;

/// The wire colour of a model colour label.
#[must_use]
pub const fn wire_color(color: fm_model::InputColorLabel) -> InputColorLabel {
    match color {
        fm_model::InputColorLabel::Red => InputColorLabel::Red,
        fm_model::InputColorLabel::Orange => InputColorLabel::Orange,
        fm_model::InputColorLabel::Yellow => InputColorLabel::Yellow,
        fm_model::InputColorLabel::Green => InputColorLabel::Green,
        fm_model::InputColorLabel::Teal => InputColorLabel::Teal,
        fm_model::InputColorLabel::Blue => InputColorLabel::Blue,
        fm_model::InputColorLabel::Purple => InputColorLabel::Purple,
        fm_model::InputColorLabel::Pink => InputColorLabel::Pink,
    }
}

/// The model colour label of a wire colour.
#[must_use]
pub const fn model_color(color: InputColorLabel) -> fm_model::InputColorLabel {
    match color {
        InputColorLabel::Red => fm_model::InputColorLabel::Red,
        InputColorLabel::Orange => fm_model::InputColorLabel::Orange,
        InputColorLabel::Yellow => fm_model::InputColorLabel::Yellow,
        InputColorLabel::Green => fm_model::InputColorLabel::Green,
        InputColorLabel::Teal => fm_model::InputColorLabel::Teal,
        InputColorLabel::Blue => fm_model::InputColorLabel::Blue,
        InputColorLabel::Purple => fm_model::InputColorLabel::Purple,
        InputColorLabel::Pink => fm_model::InputColorLabel::Pink,
    }
}

/// The wire form of a replaceable source.
#[must_use]
pub fn wire_source(source: &InputSourceSpec) -> WireInputSource {
    match source {
        InputSourceSpec::Generator { video, tone_hz } => WireInputSource::Generator {
            video: match video {
                GeneratorVideo::Solid { rgba } => WireGeneratorVideo::Solid { rgba: *rgba },
                GeneratorVideo::Bars => WireGeneratorVideo::Bars,
            },
            tone_hz: *tone_hz,
        },
        // A document travels as a media URI; its extension tells them apart
        // (IN-014).
        InputSourceSpec::Media { asset_uri } | InputSourceSpec::Document { asset_uri } => {
            WireInputSource::Media {
                uri: asset_uri.clone(),
            }
        }
        InputSourceSpec::Device { stable_key } => WireInputSource::Device {
            stable_key: stable_key.clone(),
        },
        InputSourceSpec::Network(config) => WireInputSource::Network {
            endpoint: config.url.clone(),
        },
        InputSourceSpec::SystemAudio(config) => WireInputSource::SystemAudio(config.clone()),
        InputSourceSpec::Image(config) => WireInputSource::Image(config.clone()),
        InputSourceSpec::Slideshow(config) => WireInputSource::Slideshow(Box::new(config.clone())),
        InputSourceSpec::ScreenCapture(config) => WireInputSource::ScreenCapture(config.clone()),
        InputSourceSpec::Replay => WireInputSource::Replay,
    }
}

/// The model form of a wire source.
#[must_use]
pub fn model_source(source: &WireInputSource) -> InputSourceSpec {
    match source {
        WireInputSource::Generator { video, tone_hz } => InputSourceSpec::Generator {
            video: match video {
                WireGeneratorVideo::Solid { rgba } => GeneratorVideo::Solid { rgba: *rgba },
                WireGeneratorVideo::Bars => GeneratorVideo::Bars,
            },
            tone_hz: *tone_hz,
        },
        WireInputSource::Media { uri } => InputSourceSpec::from_asset_uri(uri),
        WireInputSource::Device { stable_key } => InputSourceSpec::Device {
            stable_key: stable_key.clone(),
        },
        // A replaced network source takes the new URL with default
        // settings; `input_stream_set` edits them (IN-015).
        WireInputSource::Network { endpoint } => InputSourceSpec::Network(
            fm_types::network_stream::NetworkStreamConfig::new(endpoint.clone()),
        ),
        WireInputSource::SystemAudio(config) => InputSourceSpec::SystemAudio(config.clone()),
        WireInputSource::Image(config) => InputSourceSpec::Image(config.clone()),
        WireInputSource::Slideshow(config) => InputSourceSpec::Slideshow((**config).clone()),
        WireInputSource::ScreenCapture(config) => InputSourceSpec::ScreenCapture(config.clone()),
        WireInputSource::Replay => InputSourceSpec::Replay,
    }
}

const fn wire_refusal(refusal: BatchImportRefusal) -> ImportRefusal {
    match refusal {
        BatchImportRefusal::InvalidUri => ImportRefusal::InvalidUri,
        BatchImportRefusal::UnsupportedType => ImportRefusal::UnsupportedType,
        BatchImportRefusal::Unavailable => ImportRefusal::Unavailable,
        BatchImportRefusal::DuplicateSource => ImportRefusal::DuplicateSource,
        BatchImportRefusal::DuplicateInput => ImportRefusal::DuplicateInput,
        BatchImportRefusal::InvalidName => ImportRefusal::InvalidName,
        BatchImportRefusal::DuplicateName => ImportRefusal::DuplicateName,
        BatchImportRefusal::TooManyInputs => ImportRefusal::TooManyInputs,
    }
}

/// The wire offline reason of a media fault.
#[must_use]
pub const fn media_offline_reason(fault: EngineMediaFault) -> InputOfflineReason {
    match fault {
        EngineMediaFault::SourceMissing => InputOfflineReason::SourceMissing,
        EngineMediaFault::SourceTruncated
        | EngineMediaFault::SourceReplaced
        | EngineMediaFault::SourceRewritten
        | EngineMediaFault::SourceChanged => InputOfflineReason::SourceChanged,
        EngineMediaFault::Unsupported => InputOfflineReason::Unsupported,
        EngineMediaFault::DecodeFailed => InputOfflineReason::DecodeFailed,
        EngineMediaFault::ToolUnavailable => InputOfflineReason::ToolUnavailable,
    }
}

fn category_id(id: u32) -> InputCategoryId {
    // The codec refuses category id 0; an engine command built from any
    // other source names a category that simply does not exist.
    InputCategoryId::new(id).unwrap_or_else(|| InputCategoryId::new(u32::MAX).expect("nonzero"))
}

/// The engine edit of a wire operation.
#[must_use]
pub fn engine_edit(operation: &InputLifecycleOperation) -> EngineInputLifecycleEdit {
    match operation {
        InputLifecycleOperation::Duplicate {
            source,
            input,
            name,
        } => EngineInputLifecycleEdit::Duplicate {
            source: source.to_domain(),
            input: input.to_domain(),
            name: name.clone(),
        },
        InputLifecycleOperation::ReplaceSource { input, source } => {
            EngineInputLifecycleEdit::ReplaceSource {
                input: input.to_domain(),
                source: model_source(source),
            }
        }
        InputLifecycleOperation::SetCategory { input, category } => {
            EngineInputLifecycleEdit::SetCategory {
                input: input.to_domain(),
                category: category.map(category_id),
            }
        }
        InputLifecycleOperation::SetColor { input, color } => EngineInputLifecycleEdit::SetColor {
            input: input.to_domain(),
            color: color.map(model_color),
        },
        InputLifecycleOperation::SetPaused { input, paused } => {
            EngineInputLifecycleEdit::SetPaused {
                input: input.to_domain(),
                paused: *paused,
            }
        }
        InputLifecycleOperation::SetClosed { input, closed } => {
            EngineInputLifecycleEdit::SetClosed {
                input: input.to_domain(),
                closed: *closed,
            }
        }
        InputLifecycleOperation::SetCategories { categories } => {
            EngineInputLifecycleEdit::SetCategories {
                categories: categories
                    .iter()
                    .map(|category| InputCategory::new(category_id(category.id), &category.name))
                    .collect(),
            }
        }
        InputLifecycleOperation::Import { items } => EngineInputLifecycleEdit::Import {
            items: items
                .iter()
                .map(|item| BatchImportRequest {
                    input: item.input.to_domain(),
                    name: item.name.clone(),
                    asset_uri: item.uri.clone(),
                })
                .collect(),
        },
        InputLifecycleOperation::RemoveInputs { inputs } => {
            EngineInputLifecycleEdit::RemoveInputs {
                inputs: inputs.iter().map(|input| input.to_domain()).collect(),
            }
        }
    }
}

/// The wire operation of an engine edit.
#[must_use]
pub fn wire_operation(edit: &EngineInputLifecycleEdit) -> InputLifecycleOperation {
    let wire = |input: InputId| WireInputId::from_domain(input);
    match edit {
        EngineInputLifecycleEdit::Duplicate {
            source,
            input,
            name,
        } => InputLifecycleOperation::Duplicate {
            source: wire(*source),
            input: wire(*input),
            name: name.clone(),
        },
        EngineInputLifecycleEdit::ReplaceSource { input, source } => {
            InputLifecycleOperation::ReplaceSource {
                input: wire(*input),
                source: wire_source(source),
            }
        }
        EngineInputLifecycleEdit::SetCategory { input, category } => {
            InputLifecycleOperation::SetCategory {
                input: wire(*input),
                category: category.map(InputCategoryId::get),
            }
        }
        EngineInputLifecycleEdit::SetColor { input, color } => InputLifecycleOperation::SetColor {
            input: wire(*input),
            color: color.map(wire_color),
        },
        EngineInputLifecycleEdit::SetPaused { input, paused } => {
            InputLifecycleOperation::SetPaused {
                input: wire(*input),
                paused: *paused,
            }
        }
        EngineInputLifecycleEdit::SetClosed { input, closed } => {
            InputLifecycleOperation::SetClosed {
                input: wire(*input),
                closed: *closed,
            }
        }
        EngineInputLifecycleEdit::SetCategories { categories } => {
            InputLifecycleOperation::SetCategories {
                categories: wire_category_list(categories),
            }
        }
        EngineInputLifecycleEdit::Import { items } => InputLifecycleOperation::Import {
            items: items
                .iter()
                .map(|item| ImportItemSpec {
                    input: wire(item.input),
                    name: item.name.clone(),
                    uri: item.asset_uri.clone(),
                })
                .collect(),
        },
        EngineInputLifecycleEdit::RemoveInputs { inputs } => {
            InputLifecycleOperation::RemoveInputs {
                inputs: inputs.iter().copied().map(wire).collect(),
            }
        }
    }
}

fn wire_category_list(categories: &[InputCategory]) -> Vec<InputCategoryStatus> {
    categories
        .iter()
        .map(|category| InputCategoryStatus {
            id: category.id.get(),
            name: category.name.clone(),
        })
        .collect()
}

/// The show's category list on the wire.
#[must_use]
pub fn wire_categories(show: &ShowState) -> Vec<InputCategoryStatus> {
    wire_category_list(show.input_categories())
}

/// One input's lifecycle on the wire, with its runtime offline reason.
#[must_use]
pub fn wire_lifecycle(
    show: &ShowState,
    input: InputId,
    offline: &BTreeMap<InputId, InputOfflineReason>,
) -> InputLifecycleStatus {
    let lifecycle = show.input_lifecycle(input);
    InputLifecycleStatus {
        category: lifecycle.category.map(InputCategoryId::get),
        color: lifecycle.color.map(wire_color),
        paused: lifecycle.paused,
        closed: lifecycle.closed,
        offline: offline.get(&input).copied(),
    }
}

/// The `input_lifecycle_changed` event of an accepted edit.
#[must_use]
pub fn lifecycle_changed(
    edit: &EngineInputLifecycleEdit,
    added: &[(InputId, String)],
    refused: &[(usize, BatchImportRefusal)],
) -> EventPayload {
    let mut operation = wire_operation(edit);
    // The event reports a network URL redacted (IN-015).
    if let InputLifecycleOperation::ReplaceSource {
        source: WireInputSource::Network { endpoint },
        ..
    } = &mut operation
    {
        *endpoint = fm_types::network_stream::redact_stream_url(endpoint);
    }
    EventPayload::InputLifecycleChanged {
        operation,
        added: added
            .iter()
            .map(|(input, name)| AddedInput {
                input: WireInputId::from_domain(*input),
                name: name.clone(),
            })
            .collect(),
        refused: refused
            .iter()
            .map(|(index, refusal)| {
                (
                    u16::try_from(*index).unwrap_or(u16::MAX),
                    wire_refusal(*refusal),
                )
            })
            .collect(),
    }
}
