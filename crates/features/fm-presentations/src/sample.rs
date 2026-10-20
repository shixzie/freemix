//! Small, valid sample documents built in memory, for tests, demos, and
//! diagnostics: a PDF of solid-colour pages and a `.pptx`/`.odp` package
//! with presenter notes. They exercise the real parsers and renderer; no
//! fixture files are needed.

use std::fmt::Write as _;
use std::io::{Cursor, Write};

/// One solid page: its size in points and its fill colour.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SolidPage {
    pub width: u32,
    pub height: u32,
    pub rgb: [u8; 3],
}

/// A PDF whose pages are each filled with one colour, with `title` in its
/// document information dictionary.
#[must_use]
pub fn solid_pages_pdf(title: Option<&str>, pages: &[SolidPage]) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    // 1: catalog, 2: pages, 3: info, then a page and a content per page.
    let kids: Vec<String> = (0..pages.len())
        .map(|index| format!("{} 0 R", 4 + index * 2))
        .collect();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_owned());
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    ));
    let escaped: String = title
        .unwrap_or("")
        .chars()
        .flat_map(|c| match c {
            '(' | ')' | '\\' => vec!['\\', c],
            other => vec![other],
        })
        .collect();
    objects.push(format!("<< /Title ({escaped}) /Producer (FreeMix) >>"));
    for (index, page) in pages.iter().enumerate() {
        let [red, green, blue] = page.rgb.map(|channel| f64::from(channel) / 255.0);
        let content = format!(
            "{red:.4} {green:.4} {blue:.4} rg 0 0 {} {} re f",
            page.width, page.height
        );
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {} 0 R /Resources << >> >>",
            page.width,
            page.height,
            5 + index * 2
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        let _ = write!(pdf, "{} 0 obj\n{object}\nendobj\n", index + 1);
    }
    let xref = pdf.len();
    let _ = write!(pdf, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        let _ = writeln!(pdf, "{offset:010} 00000 n ");
    }
    let _ = write!(
        pdf,
        "trailer\n<< /Size {} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    pdf.into_bytes()
}

/// One slide of a sample presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleSlide<'a> {
    /// Presenter notes, one paragraph per line.
    pub notes: Option<&'a str>,
    pub hidden: bool,
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(crate) fn zip_package(parts: &[(String, String)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in parts {
        writer
            .start_file(name.as_str(), options)
            .expect("in-memory zip entries start");
        writer
            .write_all(body.as_bytes())
            .expect("in-memory zip entries write");
    }
    writer
        .finish()
        .expect("in-memory zip finishes")
        .into_inner()
}

/// A minimal `.pptx` package: `slides` in order, each with optional notes,
/// and `title` in its core properties.
#[must_use]
pub fn pptx(title: Option<&str>, slides: &[SampleSlide<'_>]) -> Vec<u8> {
    let mut parts = Vec::new();
    let mut ids = String::new();
    let mut rels = String::new();
    for (index, slide) in slides.iter().enumerate() {
        let number = index + 1;
        let _ = write!(
            ids,
            r#"<p:sldId id="{}" r:id="rId{number}"/>"#,
            255 + number
        );
        let _ = write!(
            rels,
            r#"<Relationship Id="rId{number}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide{number}.xml"/>"#
        );
        let show = if slide.hidden { r#" show="0""# } else { "" };
        parts.push((
            format!("ppt/slides/slide{number}.xml"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"{show}><p:cSld><p:spTree/></p:cSld></p:sld>"#
            ),
        ));
        if let Some(notes) = slide.notes {
            parts.push((
                format!("ppt/slides/_rels/slide{number}.xml.rels"),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide{number}.xml"/></Relationships>"#
                ),
            ));
            let mut paragraphs = String::new();
            for line in notes.lines() {
                let _ = write!(
                    paragraphs,
                    "<a:p><a:r><a:t>{}</a:t></a:r></a:p>",
                    xml_escape(line)
                );
            }
            parts.push((
                format!("ppt/notesSlides/notesSlide{number}.xml"),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><p:notes xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image"/><p:cNvSpPr/><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody>{paragraphs}</p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="4" name="Number"/><p:cNvSpPr/><p:nvPr><p:ph type="sldNum" idx="5"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{number}</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#
                ),
            ));
        }
    }
    parts.push((
        "ppt/presentation.xml".to_owned(),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst>{ids}</p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#
        ),
    ));
    parts.push((
        "ppt/_rels/presentation.xml.rels".to_owned(),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#
        ),
    ));
    if let Some(title) = title {
        parts.push((
            "docProps/core.xml".to_owned(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{}</dc:title></cp:coreProperties>"#,
                xml_escape(title)
            ),
        ));
    }
    parts.push((
        "[Content_Types].xml".to_owned(),
        r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#.to_owned(),
    ));
    zip_package(&parts)
}

/// A minimal `.odp` package: one `draw:page` per slide with its notes.
#[must_use]
pub fn odp(title: Option<&str>, slides: &[SampleSlide<'_>]) -> Vec<u8> {
    let mut pages = String::new();
    for (index, slide) in slides.iter().enumerate() {
        let notes = slide.notes.map_or_else(String::new, |notes| {
            let mut paragraphs = String::new();
            for line in notes.lines() {
                let _ = write!(paragraphs, "<text:p>{}</text:p>", xml_escape(line));
            }
            format!(
                "<presentation:notes><draw:frame><draw:text-box>{paragraphs}</draw:text-box></draw:frame></presentation:notes>"
            )
        });
        let _ = write!(
            pages,
            r#"<draw:page draw:name="page{}"><draw:frame><draw:text-box><text:p>Slide body</text:p></draw:text-box></draw:frame>{notes}</draw:page>"#,
            index + 1
        );
    }
    let mut parts = vec![
        (
            "mimetype".to_owned(),
            "application/vnd.oasis.opendocument.presentation".to_owned(),
        ),
        (
            "content.xml".to_owned(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"><office:body><office:presentation>{pages}</office:presentation></office:body></office:document-content>"#
            ),
        ),
    ];
    if let Some(title) = title {
        parts.push((
            "meta.xml".to_owned(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/"><office:meta><dc:title>{}</dc:title></office:meta></office:document-meta>"#,
                xml_escape(title)
            ),
        ));
    }
    zip_package(&parts)
}
