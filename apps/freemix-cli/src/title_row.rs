//! `title-apply-row`: writes one mapped data row onto a project title (GX-005).
//!
//! The row is a flat JSON object keyed by template field name. Values are
//! coerced with `fm-data`'s title coercion rules (the same rules the daemon's
//! data bindings use) and written through `fm-title-render`'s mapping bridge
//! onto the project's stored field set, so a row that names an unknown field,
//! carries an invalid image URI, or fails coercion leaves the bundle
//! untouched. `--keep-live` keeps the stored value of each failed field and
//! applies the rest.

use std::{collections::BTreeMap, fs, path::Path};

use fm_data::{
    CoercionPolicy, DataValue, ParseLimits, TitleFieldKind, TitleFieldReport, TitleFieldStatus,
    TitleMappingReport, coerce_title_value, parse_json,
};
use fm_model::{InputKind, TitleFieldEntry, TitleFieldId, TitleInput};
use fm_persistence::{ProjectStore, StoredProject};
use fm_titles::{FieldType, TitleFieldSet, TitleTemplate};
use fm_types::InputId;

use crate::app::{AppFailure, AppResult, input_id, load_stored_project};

/// Applies `row_path` onto title input `input` of the bundle at `path`.
pub fn apply_row(path: &Path, input: u128, row_path: &Path, keep_live: bool) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let input = input_id(input)?;
    let (template_uri, title) = title_input(stored.project(), input)?;
    let store = ProjectStore::new(path)?;
    let stored_template = store
        .load_title_template(template_uri)
        .map_err(|error| AppFailure(format!("title template {template_uri}: {error}")))?;
    let template = &stored_template.template;

    let row = read_row(row_path)?;
    check_unknown_fields(&row, template, template_uri)?;
    let report = mapped_report(&row, template);
    print_report(&report);
    let errors = report.errors().count();
    if errors > 0 && !keep_live {
        return Err(AppFailure(format!(
            "{errors} field(s) failed; nothing was saved (use --keep-live to keep their live values)"
        ))
        .into());
    }

    let mut fields = restored_fields(template, title, input)?;
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
    store.save(&configured)?;
    println!(
        "title-apply-row: input={input} fields={} revision={revision}",
        fields.definitions().len()
    );
    Ok(())
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

fn read_row(path: &Path) -> AppResult<BTreeMap<String, DataValue>> {
    let bytes = fs::read(path).map_err(|error| {
        AppFailure(format!("row {} could not be read: {error}", path.display()))
    })?;
    let row = parse_json(&bytes, &ParseLimits::default())
        .map_err(|error| AppFailure(format!("row {}: {error}", path.display())))?;
    let DataValue::Object(row) = row else {
        return Err(AppFailure(format!(
            "row {} must be a JSON object keyed by field name",
            path.display()
        ))
        .into());
    };
    Ok(row)
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
