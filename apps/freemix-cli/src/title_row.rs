//! `title-apply-row`: writes one mapped data row onto a project title (GX-005).
//!
//! The row is read from a flat JSON object, a CSV/TSV table, or the first or
//! named worksheet/table of an `.xlsx` or `.ods` workbook, and is keyed by
//! template field name. `--row` selects a 1-based data row in a table (JSON
//! holds exactly one row). Values are
//! coerced with `fm-data`'s title coercion rules (the same rules the daemon's
//! data bindings use) and written through `fm-title-render`'s mapping bridge
//! onto the project's stored field set, so a row that names an unknown field,
//! carries an invalid image URI, or fails coercion leaves the bundle
//! untouched. `--keep-live` keeps the stored value of each failed field and
//! applies the rest.

use std::{collections::BTreeMap, net::SocketAddr, path::Path};

use fm_data::{
    CoercionPolicy, DataValue, TitleFieldKind, TitleFieldReport, TitleFieldStatus,
    TitleMappingReport, coerce_title_value,
};
use fm_model::{InputKind, TitleFieldEntry, TitleFieldId, TitleInput};
use fm_persistence::{ProjectStore, StoredProject, StoredTitleTemplate};
use fm_protocol::{TitleAirRequirement, TitleFieldUpdate, TitleFieldValue as WireTitleFieldValue};
use fm_titles::{FieldType, FieldValue, TitleFieldSet, TitleTemplate};
use fm_types::InputId;

use crate::app::{AppFailure, AppResult, input_id, load_stored_project};
use crate::table_row::read_row;

/// A row mapped onto the title template it names, before any write.
struct MappedRow {
    template: StoredTitleTemplate,
    template_uri: String,
    report: TitleMappingReport,
}

/// Applies `row_path` onto title input `input` of the bundle at `path`, or
/// reports the per-field changes without saving when `preview` is set.
pub fn apply_row(
    path: &Path,
    input: u128,
    row_path: &Path,
    row_number: u64,
    keep_live: bool,
    preview: bool,
) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let input = input_id(input)?;
    let MappedRow {
        template: stored_template,
        template_uri: _,
        report,
    } = mapped_row(path, &stored, input, row_path, row_number)?;
    let template = &stored_template.template;
    let (_, title) = title_input(stored.project(), input)?;
    print_report(&report);
    let mut fields = restored_fields(template, title, input)?;
    if preview {
        print_preview(&fields, &report, input);
        return Ok(());
    }
    let errors = report.errors().count();
    if errors > 0 && !keep_live {
        return Err(AppFailure(format!(
            "{errors} field(s) failed; nothing was saved (use --keep-live to keep their live values)"
        ))
        .into());
    }

    let revision = fm_title_render::apply_title_mapping_to_fields(
        &mut fields,
        template,
        title.field_revision,
        &report,
    )
    .map_err(|error| AppFailure(format!("title row could not be applied: {error}")))?;

    let entries = field_entries(&fields, revision);
    let mut project = stored.project().clone();
    project
        .set_title_fields(input, revision, entries)
        .map_err(|error| {
            AppFailure(format!(
                "title input {input} fields could not be stored: {error:?}"
            ))
        })?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    ProjectStore::new(path)?.save(&configured)?;
    println!(
        "title-apply-row: input={input} fields={} revision={revision}",
        fields.definitions().len()
    );
    Ok(())
}

/// Reads one row and maps it onto the template of title input `input`,
/// refusing a row that names a field the template does not define.
fn mapped_row(
    path: &Path,
    stored: &StoredProject,
    input: InputId,
    row_path: &Path,
    row_number: u64,
) -> AppResult<MappedRow> {
    let (template_uri, _) = title_input(stored.project(), input)?;
    let template_uri = template_uri.to_owned();
    let store = ProjectStore::new(path)?;
    let stored_template = store
        .load_title_template(&template_uri)
        .map_err(|error| AppFailure(format!("title template {template_uri}: {error}")))?;
    let row = read_row(row_path, row_number)?;
    check_unknown_fields(&row, &stored_template.template, &template_uri)?;
    let report = mapped_report(&row, &stored_template.template);
    Ok(MappedRow {
        template: stored_template,
        template_uri,
        report,
    })
}

/// Applies `row_path` onto the live title input `input` of the daemon at
/// `address` (GX-005), mapping against the local project's template.
///
/// The row's mapped values are addressed to the remote title's own field
/// ids by name; the remote snapshot supplies the base field revision and
/// whether the title is on air, so the same row becomes a live update or an
/// off-air prepare. `--keep-live` omits failed fields, leaving the daemon's
/// current values, and `--preview` reports the remote changes without
/// sending anything.
#[allow(clippy::too_many_arguments)] // one remote apply: address, bundle, row, and options
pub fn remote_apply_row(
    address: SocketAddr,
    path: &Path,
    input: u128,
    row_path: &Path,
    row_number: u64,
    keep_live: bool,
    preview: bool,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let input = input_id(input)?;
    let MappedRow {
        template: _,
        template_uri,
        report,
    } = mapped_row(path, &stored, input, row_path, row_number)?;
    print_report(&report);
    let remote = crate::remote::title_state(address, input)?;
    let errors = report.errors().count();
    if errors > 0 && !keep_live {
        return Err(AppFailure(format!(
            "{errors} field(s) failed; nothing was sent (use --keep-live to keep their live values)"
        ))
        .into());
    }

    let mut updates = Vec::new();
    for entry in &report.fields {
        let Some(value) = report.values.get(&entry.field) else {
            continue;
        };
        let Some(field) = remote.fields.iter().find(|field| field.name == entry.field) else {
            return Err(AppFailure(format!(
                "row names title field {:?}, which the remote title of template {template_uri} does not define",
                entry.field
            ))
            .into());
        };
        let value = wire_field_value(&mapped_field_value(value));
        if field.value == value {
            if preview {
                println!("unchanged {:?}", entry.field);
            }
            continue;
        }
        if preview {
            println!(
                "would-change {:?} {:?} -> {:?}",
                entry.field, field.value, value
            );
        }
        updates.push(TitleFieldUpdate {
            field: field.field,
            value,
        });
    }
    // The wire requires fields strictly ascending by id; a template lists
    // them in its own order (here, by name).
    updates.sort_by_key(|update| update.field);

    let require = if remote.on_air {
        TitleAirRequirement::OnAir
    } else {
        TitleAirRequirement::OffAir
    };
    if preview {
        println!(
            "title-apply-row: input={input} preview phase={:?} require={} changes={}",
            remote.phase,
            if remote.on_air { "on_air" } else { "off_air" },
            updates.len()
        );
        return Ok(());
    }
    if updates.is_empty() {
        println!("title-apply-row: input={input} nothing to change");
        return Ok(());
    }
    let revision = remote.field_revision.saturating_add(1);
    crate::remote::execute(
        address,
        fm_protocol::CommandPayload::SetTitleFields {
            input: fm_protocol::WireInputId::from_domain(input),
            base_revision: remote.field_revision,
            require,
            fields: updates,
        },
        key,
        expected_revision,
    )?;
    println!(
        "remote-title-apply-row: input={input} phase={:?} require={} revision={revision}",
        remote.phase,
        if remote.on_air { "on_air" } else { "off_air" }
    );
    Ok(())
}

/// Applies one raw data row to `input` on the daemon at `address`
/// (GX-005): the daemon owns the template, maps the strings by field name,
/// and applies the coerced values atomically. A null cell is omitted and
/// keeps the live value.
pub fn remote_apply_data(
    address: std::net::SocketAddr,
    input: u128,
    row_path: &Path,
    row_number: u64,
    keep_live: bool,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> AppResult<()> {
    let input = input_id(input)?;
    let row = read_row(row_path, row_number)?;
    let mut values = Vec::new();
    for (field, value) in row {
        if value.is_null() {
            continue;
        }
        let Some(text) = value.scalar_text() else {
            return Err(AppFailure(format!("table field {field:?} is not a scalar value")).into());
        };
        values.push(fm_protocol::TitleRowValue { field, value: text });
    }
    let remote = crate::remote::title_state(address, input)?;
    let require = if remote.on_air {
        fm_protocol::TitleAirRequirement::OnAir
    } else {
        fm_protocol::TitleAirRequirement::OffAir
    };
    if values.is_empty() {
        println!("remote-title-row: input={input} nothing to apply");
        return Ok(());
    }
    let revision = remote.field_revision.saturating_add(1);
    crate::remote::execute(
        address,
        fm_protocol::CommandPayload::SetTitleRow {
            input: fm_protocol::WireInputId::from_domain(input),
            base_revision: remote.field_revision,
            require,
            values,
            keep_live,
        },
        key,
        expected_revision,
    )?;
    println!(
        "remote-title-row: input={input} require={} revision={revision}",
        if remote.on_air { "on_air" } else { "off_air" }
    );
    Ok(())
}

fn wire_field_value(value: &FieldValue) -> WireTitleFieldValue {
    match value {
        FieldValue::Text(text) => WireTitleFieldValue::Text(text.clone()),
        FieldValue::Number(number) => WireTitleFieldValue::Number(*number),
        FieldValue::Color(color) => WireTitleFieldValue::Color {
            red: color.r,
            green: color.g,
            blue: color.b,
            alpha: color.a,
        },
        FieldValue::Image(image) => WireTitleFieldValue::Image {
            asset_uri: image.asset.clone(),
        },
    }
}

fn title_input(project: &fm_model::Project, input: InputId) -> AppResult<(&str, &TitleInput)> {
    let target = project
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input)
        .ok_or_else(|| AppFailure(format!("title input {input} does not exist")))?;
    let InputKind::Title(title) = &target.kind else {
        return Err(AppFailure(format!("input {input} is not a title input")).into());
    };
    Ok((title.template_uri.as_str(), title))
}

fn check_unknown_fields(
    row: &BTreeMap<String, DataValue>,
    template: &TitleTemplate,
    template_uri: &str,
) -> AppResult<()> {
    if let Some(unknown) = row
        .keys()
        .find(|name| !template.fields.iter().any(|field| field.name == **name))
    {
        return Err(AppFailure(format!(
            "row names title field {unknown:?}, which template {template_uri} does not define"
        ))
        .into());
    }
    Ok(())
}

fn mapped_report(
    row: &BTreeMap<String, DataValue>,
    template: &TitleTemplate,
) -> TitleMappingReport {
    let policy = CoercionPolicy::default();
    let mut values = BTreeMap::new();
    let mut reports = Vec::with_capacity(template.fields.len());
    for field in &template.fields {
        let status = match row.get(&field.name) {
            None | Some(DataValue::Null) => TitleFieldStatus::Missing,
            Some(value) => {
                match coerce_title_value(value, title_field_kind(field.field_type()), &policy) {
                    Ok(value) => {
                        values.insert(field.name.clone(), value);
                        TitleFieldStatus::Mapped
                    }
                    Err(error) => TitleFieldStatus::Coercion(error),
                }
            }
        };
        reports.push(TitleFieldReport {
            field: field.name.clone(),
            status,
        });
    }
    TitleMappingReport {
        values,
        fields: reports,
    }
}

fn restored_fields(
    template: &TitleTemplate,
    title: &TitleInput,
    input: InputId,
) -> AppResult<TitleFieldSet> {
    Ok(TitleFieldSet::restore_revisioned(
        template,
        title.field_revision,
        title.fields.iter().map(|entry| {
            (
                fm_titles::FieldId::new(entry.id.get()),
                fm_title_render::field_value(&entry.value),
                entry.revision,
            )
        }),
    )
    .map_err(|error| {
        AppFailure(format!(
            "stored fields of title input {input} do not match template {}: {error:?}",
            title.template_uri
        ))
    })?)
}

fn field_entries(fields: &TitleFieldSet, revision: u64) -> Vec<TitleFieldEntry> {
    fields
        .values()
        .iter()
        .map(|(field, value)| TitleFieldEntry {
            id: TitleFieldId::new(field.get()),
            value: fm_title_render::model_field_value(value),
            revision: fields.field_revision(*field).unwrap_or(revision),
        })
        .collect()
}

fn print_report(report: &TitleMappingReport) {
    for entry in &report.fields {
        println!(
            "field {:?} {}",
            entry.field,
            field_status_name(&entry.status)
        );
    }
}

fn mapped_field_value(value: &fm_data::TitleFieldValue) -> fm_titles::FieldValue {
    match value {
        fm_data::TitleFieldValue::Text(text) => fm_titles::FieldValue::Text(text.clone()),
        fm_data::TitleFieldValue::Number(number) => fm_titles::FieldValue::Number(*number),
        fm_data::TitleFieldValue::Color(color) => {
            fm_titles::FieldValue::Color(fm_titles::Color::new(color.r, color.g, color.b, color.a))
        }
        fm_data::TitleFieldValue::Image(asset) => {
            fm_titles::FieldValue::Image(fm_titles::ImageSource::new(asset.clone()))
        }
    }
}

fn print_preview(fields: &TitleFieldSet, report: &TitleMappingReport, input: InputId) {
    for entry in &report.fields {
        if let Some(value) = report.values.get(&entry.field) {
            let new = mapped_field_value(value);
            let current = fields
                .definitions()
                .iter()
                .find(|definition| definition.name == entry.field)
                .and_then(|definition| fields.value(definition.id));
            if current == Some(&new) {
                println!("unchanged {:?}", entry.field);
            } else {
                println!("would-change {:?} {current:?} -> {new:?}", entry.field);
            }
        } else {
            println!(
                "failed {:?} {}",
                entry.field,
                field_status_name(&entry.status)
            );
        }
    }
    println!("title-apply-row: input={input} preview");
}

const fn title_field_kind(field_type: FieldType) -> TitleFieldKind {
    match field_type {
        FieldType::Text => TitleFieldKind::Text,
        FieldType::Number => TitleFieldKind::Number,
        FieldType::Color => TitleFieldKind::Color,
        FieldType::Image => TitleFieldKind::Image,
    }
}

fn field_status_name(status: &TitleFieldStatus) -> String {
    match status {
        TitleFieldStatus::Mapped => "mapped".to_owned(),
        TitleFieldStatus::Fallback => "fallback".to_owned(),
        TitleFieldStatus::Omitted => "omitted".to_owned(),
        TitleFieldStatus::Missing => "missing".to_owned(),
        TitleFieldStatus::Path(error) => format!("path-error {error}"),
        TitleFieldStatus::Transform(error) => format!("transform-error {error}"),
        TitleFieldStatus::Coercion(error) => format!("coercion-error {error}"),
    }
}
