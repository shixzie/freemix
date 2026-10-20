//! Shortcut sets through files (RC-004).
//!
//! `shortcuts` prints a project's shortcuts, `shortcut-export` writes them
//! all to a `.fmshortcuts` file, and `shortcut-import` merges one into the
//! project: a shortcut whose name the project already uses is redefined
//! under its id, every other one is added under a fresh id, and nothing is
//! imported when the merged set would clash on a key or OSC name or exceed
//! the project's 64 shortcuts. An import is a project edit, refused on a
//! locked project. The `remote-` forms do the same against a running
//! daemon, whose import sends one undoable `shortcut_set` per change.

use std::path::Path;

use fm_preset_file::shortcuts::{
    ShortcutImportPlan, ShortcutSetFile, plan_shortcut_import, read_shortcut_set_file,
    write_shortcut_set_file,
};
use fm_protocol::{ShortcutStatus, shortcut_action_text};

use crate::app::{AppFailure, AppResult, inspect_stored_project, update_project};
use crate::args::ShortcutSetCommand;

/// The set name an export uses: `name`, else the show name, else
/// `Shortcuts`.
pub(crate) fn set_name(name: Option<String>, show: &str) -> String {
    name.unwrap_or_else(|| {
        let show = show.trim();
        if show.is_empty() || show.len() > fm_preset_file::shortcuts::MAX_SHORTCUT_SET_NAME_BYTES {
            "Shortcuts".to_owned()
        } else {
            show.to_owned()
        }
    })
}

/// Prints one `Shortcut(...)` line per shortcut, or `Shortcuts=none`.
pub(crate) fn print_shortcuts(shortcuts: &[ShortcutStatus]) {
    if shortcuts.is_empty() {
        println!("Shortcuts=none");
    }
    for shortcut in shortcuts {
        let key = shortcut
            .spec
            .key
            .as_ref()
            .map_or_else(|| "-".to_owned(), key_text);
        let osc = shortcut.spec.osc.as_ref().map_or_else(
            || "-".to_owned(),
            |name| format!("{}{name}", fm_protocol::SHORTCUT_OSC_PREFIX),
        );
        println!(
            "Shortcut(id={}, name={:?}, enabled={}, key={key}, osc={osc}, action={:?})",
            shortcut.id,
            shortcut.spec.name,
            shortcut.spec.enabled,
            shortcut_action_text(&shortcut.spec.action)
        );
    }
}

/// Writes `shortcuts` to `file` as set `name`.
pub(crate) fn export(
    shortcuts: &[ShortcutStatus],
    file: &Path,
    name: String,
    command: &str,
) -> AppResult<()> {
    if shortcuts.is_empty() {
        return Err(AppFailure(format!("{command}: the project has no shortcuts")).into());
    }
    let set = ShortcutSetFile {
        name,
        shortcuts: shortcuts
            .iter()
            .map(|shortcut| shortcut.spec.clone())
            .collect(),
    };
    write_shortcut_set_file(file, &set)
        .map_err(|error| AppFailure(format!("{}: {error}", file.display())))?;
    println!(
        "{command}: {} shortcuts as {:?} to {}",
        set.shortcuts.len(),
        set.name,
        file.display()
    );
    Ok(())
}

/// Reads `file` and plans its merge into `existing`.
pub(crate) fn plan(existing: &[ShortcutStatus], file: &Path) -> AppResult<ShortcutImportPlan> {
    let set = read_shortcut_set_file(file)
        .map_err(|error| AppFailure(format!("{}: {error}", file.display())))?;
    plan_shortcut_import(existing, &set.shortcuts)
        .map_err(|error| AppFailure(format!("{}: {error}", file.display())).into())
}

/// Prints an import's counts.
pub(crate) fn print_import(command: &str, plan: &ShortcutImportPlan) {
    println!(
        "{command}: added={} replaced={} unchanged={}",
        plan.added, plan.replaced, plan.unchanged
    );
}

/// Runs one shortcut set command on the local project at `path`.
pub(crate) fn local(path: &Path, command: ShortcutSetCommand) -> AppResult<()> {
    match command {
        ShortcutSetCommand::List => {
            let stored = inspect_stored_project(path)?;
            let shortcuts = wire(stored.project().shortcuts());
            print_shortcuts(&shortcuts);
            Ok(())
        }
        ShortcutSetCommand::Export { file, name } => {
            let stored = inspect_stored_project(path)?;
            let project = stored.project();
            export(
                &wire(project.shortcuts()),
                &file,
                set_name(name, project.name()),
                "shortcut-export",
            )
        }
        ShortcutSetCommand::Import { file } => update_project(path, |project| {
            if project.project_locked() {
                return Err(AppFailure("project is locked".into()).into());
            }
            let plan = plan(&wire(project.shortcuts()), &file)?;
            let mut shortcuts = project.shortcuts().to_vec();
            for set in &plan.sets {
                let shortcut = fm_control::domain_shortcut(set.id, &set.spec);
                match shortcuts.iter_mut().find(|found| found.id == set.id) {
                    Some(found) => *found = shortcut,
                    None => shortcuts.push(shortcut),
                }
            }
            fm_model::validate_shortcuts(&shortcuts)
                .map_err(|error| AppFailure(format!("{}: {error}", file.display())))?;
            project.set_shortcuts(shortcuts);
            print_import("shortcut-import", &plan);
            Ok(())
        }),
    }
}

/// `Ctrl+Shift+Alt+F9`: a key as the console writes it.
/// `Ctrl+Shift+Alt+F9`, a chord `Ctrl+K,1`, a scoped key `M@mixer`.
fn key_text(key: &fm_protocol::WireShortcutKey) -> String {
    let stroke = |key: &fm_protocol::WireShortcutKey| {
        let mut text = String::new();
        for (held, name) in [
            (key.command, "Ctrl+"),
            (key.shift, "Shift+"),
            (key.alt, "Alt+"),
        ] {
            if held {
                text.push_str(name);
            }
        }
        text.push_str(&key.key);
        text
    };
    let mut text = stroke(key);
    if let Some(then) = &key.then {
        text.push(',');
        text.push_str(&stroke(then));
    }
    if let Some(scope) = &key.scope {
        text.push('@');
        text.push_str(scope);
    }
    text
}

fn wire(shortcuts: &[fm_model::Shortcut]) -> Vec<ShortcutStatus> {
    shortcuts.iter().map(fm_control::wire_shortcut).collect()
}
