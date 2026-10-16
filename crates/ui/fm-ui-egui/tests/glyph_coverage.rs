//! Every non-ASCII character the console can draw exists in its fonts.
//!
//! egui draws a missing glyph as an empty box (`◻`), which reads as a
//! broken key (the Layer designer's effect rows once showed `▲ ▼ ✕` as
//! boxes). This test collects every non-ASCII character outside comments in
//! the UI crates and Studio, and checks that the console's proportional
//! family (whose fallbacks the monospace family also carries) has it.
//! Symbols the fonts lack are painted instead (`theme::KeyIcon`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The atlas region a one-character galley draws, as text.
fn glyph_region(context: &egui::Context, c: char) -> String {
    let galley = context.fonts_mut(|fonts| {
        fonts.layout_no_wrap(
            c.to_string(),
            egui::FontId::proportional(12.0),
            egui::Color32::WHITE,
        )
    });
    galley
        .rows
        .first()
        .and_then(|row| row.row.glyphs.first())
        .map_or_else(String::new, |glyph| format!("{:?}", glyph.uv_rect))
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_drawn_character_is_in_the_console_fonts() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../../..");
    let mut files = Vec::new();
    for dir in [
        "crates/ui/fm-ui-egui/src",
        "crates/ui/fm-ui-model/src",
        "apps/freemix-studio/src",
    ] {
        rust_sources(&root.join(dir), &mut files);
    }
    assert!(files.len() > 20, "found only {} sources", files.len());
    // First place each character appears.
    let mut used: BTreeMap<char, String> = BTreeMap::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("readable source");
        for (number, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for c in line.chars().filter(|c| !c.is_ascii() && !c.is_whitespace()) {
                used.entry(c)
                    .or_insert_with(|| format!("{}:{}", file.display(), number + 1));
            }
        }
    }
    assert!(used.contains_key(&'·'), "the scan saw no `·`");

    let context = egui::Context::default();
    let _ = context.run_ui(egui::RawInput::default(), |_| {});
    // A private-use character no font has: what a missing glyph draws.
    let missing = glyph_region(&context, '\u{10FFFD}');
    let absent: Vec<String> = used
        .iter()
        .filter(|(c, _)| glyph_region(&context, **c) == missing)
        .map(|(c, at)| format!("U+{:04X} {c} at {at}", u32::from(*c)))
        .collect();
    assert!(
        absent.is_empty(),
        "characters missing from the console fonts (they draw as empty boxes; \
         paint them or use a covered glyph):\n{}",
        absent.join("\n")
    );
}
