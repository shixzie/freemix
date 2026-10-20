//! Presentation package metadata read without an office suite (IN-014).
//!
//! A `PowerPoint` (`.pptx`) or `OpenDocument` (`.odp`) presentation is a zip
//! package of XML parts. Its slide count, title, and presenter notes are
//! read here in pure Rust, so a presentation input knows its pages and the
//! presenter sees each slide's notes even on a server without
//! `LibreOffice`; only drawing the slides needs the conversion to PDF (see
//! [`crate::convert`]).
//!
//! - `.pptx`: the slide order is `ppt/presentation.xml`'s `sldIdLst`,
//!   resolved through `ppt/_rels/presentation.xml.rels`. A hidden slide
//!   (`<p:sld show="0">`) is skipped, as the PDF export skips it. A slide's
//!   notes are the text of the body placeholder of the notes slide its
//!   relationships name, one line per paragraph. The title is
//!   `docProps/core.xml`'s `dc:title`.
//! - `.odp`: every `draw:page` of `content.xml`'s `office:presentation`, in
//!   order; its notes are the paragraphs of its `presentation:notes`. The
//!   title is `meta.xml`'s `dc:title`.
//!
//! Every part read is bounded ([`MAX_PART_BYTES`]), so a hostile package
//! cannot exhaust memory.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use fm_types::paged::{
    DocumentFormat, MAX_PAGED_NOTES_BYTES, MAX_PAGED_PAGES, MAX_PAGED_TITLE_BYTES,
    bounded_paged_text,
};
use quick_xml::Reader;
use quick_xml::events::{BytesRef, BytesStart, Event};

use crate::document::{DocumentError, DocumentInfo};

/// Largest uncompressed XML part read from a package.
pub const MAX_PART_BYTES: u64 = 16 * 1024 * 1024;

/// Reads the slide count, title, and notes of a `.pptx` or `.odp` package.
///
/// # Errors
///
/// [`DocumentError::Malformed`] when the bytes are not such a package or a
/// required part is missing or not XML, and
/// [`DocumentError::TooManyPages`] past [`MAX_PAGED_PAGES`] slides.
pub fn probe_presentation(
    format: DocumentFormat,
    bytes: &[u8],
) -> Result<DocumentInfo, DocumentError> {
    let mut package = Package::open(bytes)?;
    let (title, notes) = match format {
        DocumentFormat::Pptx => (
            package
                .optional("docProps/core.xml")?
                .and_then(|xml| dc_title(&xml)),
            pptx_notes(&mut package)?,
        ),
        DocumentFormat::Odp => (
            package.optional("meta.xml")?.and_then(|xml| dc_title(&xml)),
            odp_notes(&package.required("content.xml")?)?,
        ),
        DocumentFormat::Pdf | DocumentFormat::Keynote => {
            return Err(DocumentError::Unsupported(format));
        }
    };
    if notes.is_empty() {
        return Err(DocumentError::Malformed(
            "the presentation has no slides".into(),
        ));
    }
    let page_count =
        u32::try_from(notes.len()).map_err(|_| DocumentError::TooManyPages(u32::MAX))?;
    if page_count > MAX_PAGED_PAGES {
        return Err(DocumentError::TooManyPages(page_count));
    }
    Ok(DocumentInfo {
        format,
        page_count,
        title,
        notes,
    })
}

struct Package<'a> {
    archive: zip::ZipArchive<Cursor<&'a [u8]>>,
}

impl<'a> Package<'a> {
    fn open(bytes: &'a [u8]) -> Result<Self, DocumentError> {
        zip::ZipArchive::new(Cursor::new(bytes))
            .map(|archive| Self { archive })
            .map_err(|error| {
                DocumentError::Malformed(format!("not a presentation package: {error}"))
            })
    }

    fn optional(&mut self, name: &str) -> Result<Option<String>, DocumentError> {
        let Ok(file) = self.archive.by_name(name) else {
            return Ok(None);
        };
        let mut text = String::new();
        file.take(MAX_PART_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|error| DocumentError::Malformed(format!("{name} cannot be read: {error}")))?;
        if text.len() as u64 > MAX_PART_BYTES {
            return Err(DocumentError::Malformed(format!("{name} is too large")));
        }
        Ok(Some(text))
    }

    fn required(&mut self, name: &str) -> Result<String, DocumentError> {
        self.optional(name)?
            .ok_or_else(|| DocumentError::Malformed(format!("the package has no {name}")))
    }
}

fn malformed(part: &str, error: impl std::fmt::Display) -> DocumentError {
    DocumentError::Malformed(format!("{part} is not valid XML: {error}"))
}

/// The text an entity or character reference stands for.
fn reference_text(reference: &BytesRef<'_>) -> Option<String> {
    if let Ok(Some(c)) = reference.resolve_char_ref() {
        return Some(c.to_string());
    }
    Some(
        match &**reference {
            b"lt" => "<",
            b"gt" => ">",
            b"amp" => "&",
            b"apos" => "'",
            b"quot" => "\"",
            _ => return None,
        }
        .to_owned(),
    )
}

fn attribute(element: &BytesStart<'_>, local: &[u8]) -> Option<String> {
    element.attributes().flatten().find_map(|attribute| {
        (attribute.key.local_name().as_ref() == local)
            .then(|| {
                attribute
                    .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .ok()
                    .map(std::borrow::Cow::into_owned)
            })
            .flatten()
    })
}

/// Visits every event of `xml`, with the element's local name for starts.
fn walk(
    part: &str,
    xml: &str,
    mut visit: impl FnMut(&Event<'_>) -> Result<(), DocumentError>,
) -> Result<(), DocumentError> {
    let mut reader = Reader::from_str(xml);
    loop {
        let event = reader
            .read_event()
            .map_err(|error| malformed(part, error))?;
        if matches!(event, Event::Eof) {
            return Ok(());
        }
        visit(&event)?;
    }
}

fn text_of(event: &Event<'_>) -> Option<String> {
    match event {
        Event::Text(text) => text.decode().ok().map(std::borrow::Cow::into_owned),
        Event::CData(data) => data.decode().ok().map(std::borrow::Cow::into_owned),
        Event::GeneralRef(reference) => reference_text(reference),
        _ => None,
    }
}

/// The `dc:title` of a core or meta properties part.
fn dc_title(xml: &str) -> Option<String> {
    let mut inside = false;
    let mut title = String::new();
    walk("the document properties", xml, |event| {
        match event {
            Event::Start(element) if element.local_name().as_ref() == b"title" => inside = true,
            Event::End(element) if element.local_name().as_ref() == b"title" => inside = false,
            other if inside => {
                if let Some(text) = text_of(other) {
                    title.push_str(&text);
                }
            }
            _ => {}
        }
        Ok(())
    })
    .ok()?;
    let title = bounded_paged_text(title.trim(), MAX_PAGED_TITLE_BYTES, false);
    (!title.is_empty()).then_some(title)
}

/// The relationships of a part: id to target, resolved against `base` (the
/// directory of the part that owns them).
fn relationships(
    xml: &str,
    base: &str,
) -> Result<BTreeMap<String, (String, String)>, DocumentError> {
    let mut map = BTreeMap::new();
    walk("a relationships part", xml, |event| {
        if let Event::Start(element) | Event::Empty(element) = event
            && element.local_name().as_ref() == b"Relationship"
            && let (Some(id), Some(target)) =
                (attribute(element, b"Id"), attribute(element, b"Target"))
        {
            let kind = attribute(element, b"Type").unwrap_or_default();
            map.insert(id, (kind, resolve_part(base, &target)));
        }
        Ok(())
    })?;
    Ok(map)
}

/// `target` relative to the package directory `base`, with `..` resolved.
fn resolve_part(base: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if let Some(absolute) = target.strip_prefix('/') {
        return absolute.to_owned();
    } else {
        base.split('/').filter(|part| !part.is_empty()).collect()
    };
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn directory_of(part: &str) -> &str {
    part.rsplit_once('/').map_or("", |(directory, _)| directory)
}

fn rels_path(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((directory, file)) => format!("{directory}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

/// The notes of every shown slide of a `.pptx`, in slide order.
fn pptx_notes(package: &mut Package<'_>) -> Result<Vec<String>, DocumentError> {
    let presentation = package.required("ppt/presentation.xml")?;
    let rels = relationships(&package.required("ppt/_rels/presentation.xml.rels")?, "ppt")?;
    let mut slide_ids = Vec::new();
    let mut in_list = false;
    walk("ppt/presentation.xml", &presentation, |event| {
        match event {
            Event::Start(element) if element.local_name().as_ref() == b"sldIdLst" => {
                in_list = true;
            }
            Event::End(element) if element.local_name().as_ref() == b"sldIdLst" => {
                in_list = false;
            }
            Event::Start(element) | Event::Empty(element)
                if in_list && element.local_name().as_ref() == b"sldId" =>
            {
                if let Some(id) = relationship_id(element) {
                    slide_ids.push(id);
                }
            }
            _ => {}
        }
        if slide_ids.len() as u64 > u64::from(MAX_PAGED_PAGES) {
            return Err(DocumentError::TooManyPages(MAX_PAGED_PAGES + 1));
        }
        Ok(())
    })?;
    let mut notes = Vec::with_capacity(slide_ids.len());
    for id in slide_ids {
        let Some((_, slide)) = rels.get(&id) else {
            return Err(DocumentError::Malformed(format!(
                "slide relationship {id} names no part"
            )));
        };
        let xml = package.required(slide)?;
        if slide_hidden(&xml)? {
            continue;
        }
        let notes_part = package
            .optional(&rels_path(slide))?
            .map(|xml| relationships(&xml, directory_of(slide)))
            .transpose()?
            .and_then(|rels| {
                rels.into_values()
                    .find(|(kind, _)| kind.ends_with("/notesSlide"))
                    .map(|(_, target)| target)
            });
        let text = match notes_part {
            Some(part) => match package.optional(&part)? {
                Some(xml) => pptx_body_text(&xml)?,
                None => String::new(),
            },
            None => String::new(),
        };
        notes.push(text);
    }
    Ok(notes)
}

/// The `r:id` of a `p:sldId`.
fn relationship_id(element: &BytesStart<'_>) -> Option<String> {
    element.attributes().flatten().find_map(|attribute| {
        let key = attribute.key.as_ref();
        (key.ends_with(b":id") && key != b"id")
            .then(|| {
                attribute
                    .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .ok()
                    .map(std::borrow::Cow::into_owned)
            })
            .flatten()
    })
}

/// Whether a slide part is hidden (`<p:sld show="0">`).
fn slide_hidden(xml: &str) -> Result<bool, DocumentError> {
    let mut hidden = false;
    let mut seen_root = false;
    walk("a slide", xml, |event| {
        if !seen_root && let Event::Start(element) | Event::Empty(element) = event {
            seen_root = true;
            hidden = matches!(attribute(element, b"show").as_deref(), Some("0" | "false"));
        }
        Ok(())
    })?;
    Ok(hidden)
}

/// The text of the body placeholders of a notes slide, one line per
/// paragraph, bounded.
fn pptx_body_text(xml: &str) -> Result<String, DocumentError> {
    // Shapes nest; track the shape whose placeholder is the notes body.
    let mut depth_in_shape = 0_usize;
    let mut body = false;
    let mut in_text = false;
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_paragraph = false;
    walk("a notes slide", xml, |event| {
        match event {
            Event::Start(element) => match element.local_name().as_ref() {
                b"sp" => {
                    depth_in_shape += 1;
                    if depth_in_shape == 1 {
                        body = false;
                    }
                }
                b"p" if depth_in_shape > 0 && body => {
                    in_paragraph = true;
                    current.clear();
                }
                b"t" if in_paragraph => in_text = true,
                _ => {}
            },
            Event::Empty(element) => match element.local_name().as_ref() {
                b"ph" if depth_in_shape > 0 => {
                    body = attribute(element, b"type").as_deref() == Some("body");
                }
                b"br" if in_paragraph => current.push('\n'),
                _ => {}
            },
            Event::End(element) => match element.local_name().as_ref() {
                b"sp" => depth_in_shape = depth_in_shape.saturating_sub(1),
                b"p" if in_paragraph => {
                    in_paragraph = false;
                    paragraphs.push(std::mem::take(&mut current));
                }
                b"t" => in_text = false,
                _ => {}
            },
            other if in_text => {
                if let Some(text) = text_of(other) {
                    current.push_str(&text);
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(bounded_notes(&paragraphs))
}

/// Paragraphs joined by line breaks, trailing blank lines dropped, bounded.
fn bounded_notes(paragraphs: &[String]) -> String {
    let joined = paragraphs.join("\n");
    bounded_paged_text(joined.trim_end(), MAX_PAGED_NOTES_BYTES, true)
}

/// The notes of every page of an `.odp`'s `content.xml`, in order.
fn odp_notes(xml: &str) -> Result<Vec<String>, DocumentError> {
    let mut pages = Vec::new();
    let mut in_page = false;
    let mut in_notes = 0_usize;
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut paragraph_depth = 0_usize;
    walk("content.xml", xml, |event| {
        match event {
            Event::Start(element) => match element.local_name().as_ref() {
                b"page" if !in_page => {
                    in_page = true;
                    paragraphs.clear();
                }
                b"notes" if in_page => in_notes += 1,
                b"p" | b"h" if in_notes > 0 => {
                    paragraph_depth += 1;
                    if paragraph_depth == 1 {
                        current.clear();
                    }
                }
                _ => {}
            },
            Event::Empty(element) => match element.local_name().as_ref() {
                b"page" if !in_page => pages.push(String::new()),
                b"line-break" if paragraph_depth > 0 => current.push('\n'),
                b"tab" if paragraph_depth > 0 => current.push('\t'),
                b"s" if paragraph_depth > 0 => current.push(' '),
                _ => {}
            },
            Event::End(element) => match element.local_name().as_ref() {
                b"page" if in_page => {
                    in_page = false;
                    pages.push(bounded_notes(&paragraphs));
                }
                b"notes" if in_notes > 0 => in_notes -= 1,
                b"p" | b"h" if paragraph_depth > 0 => {
                    paragraph_depth -= 1;
                    if paragraph_depth == 0 {
                        paragraphs.push(std::mem::take(&mut current));
                    }
                }
                _ => {}
            },
            other if paragraph_depth > 0 => {
                if let Some(text) = text_of(other) {
                    current.push_str(&text);
                }
            }
            _ => {}
        }
        if pages.len() as u64 > u64::from(MAX_PAGED_PAGES) {
            return Err(DocumentError::TooManyPages(MAX_PAGED_PAGES + 1));
        }
        Ok(())
    })?;
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_resolve_against_their_directory() {
        assert_eq!(
            resolve_part("ppt", "slides/slide1.xml"),
            "ppt/slides/slide1.xml"
        );
        assert_eq!(
            resolve_part("ppt/slides", "../notesSlides/notesSlide1.xml"),
            "ppt/notesSlides/notesSlide1.xml"
        );
        assert_eq!(resolve_part("ppt", "/ppt/x.xml"), "ppt/x.xml");
        assert_eq!(
            rels_path("ppt/slides/slide1.xml"),
            "ppt/slides/_rels/slide1.xml.rels"
        );
    }

    #[test]
    fn notes_body_keeps_paragraphs_and_skips_other_placeholders() {
        let xml = r#"<p:notes xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
            <p:sp><p:nvSpPr><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp>
            <p:sp><p:nvSpPr><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr>
              <p:txBody><a:p><a:r><a:t>Welcome &amp; thanks</a:t></a:r></a:p>
              <a:p><a:r><a:t>Second</a:t></a:r><a:br/><a:r><a:t>line</a:t></a:r></a:p></p:txBody></p:sp>
            <p:sp><p:nvSpPr><p:nvPr><p:ph type="sldNum"/></p:nvPr></p:nvSpPr>
              <p:txBody><a:p><a:r><a:t>7</a:t></a:r></a:p></p:txBody></p:sp>
        </p:spTree></p:cSld></p:notes>"#;
        assert_eq!(
            pptx_body_text(xml).unwrap(),
            "Welcome & thanks\nSecond\nline"
        );
    }
}
