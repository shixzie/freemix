//! A bounded polling source that reads one file from disk (GX-004).
//!
//! The CSV, JSON, and XLSX parsers never perform I/O; this adapter supplies
//! the bytes. It reads at most [`MAX_FILE_SOURCE_BYTES`], skips a poll while
//! the file's length and modification time are unchanged, and reports read
//! and parse failures as [`SourceError::Adapter`]. A vanished file is not an
//! error: the source stays running and emits nothing until it reappears.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::adapters::DataAdapter;
use crate::ods::{parse_ods, parse_ods_sheet};
use crate::parse::{CsvOptions, ParseLimits, parse_csv, parse_json};
use crate::source::{DataSource, PollingSource, SourceError, SourceEvent, SourceState};
use crate::value::DataValue;
use crate::xlsx::{parse_xlsx, parse_xlsx_sheet};

/// The largest file a file source reads, in bytes.
pub const MAX_FILE_SOURCE_BYTES: usize = 16 * 1024 * 1024;

/// How a file source parses its bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileFormat {
    /// Comma-separated values with the given dialect and limits.
    Csv(CsvOptions),
    /// One JSON document with the given limits.
    Json(ParseLimits),
    /// The first worksheet, or the named worksheet, of an `.xlsx` workbook.
    Xlsx { sheet: Option<String> },
    /// The first table, or the named table, of an `.ods` spreadsheet.
    Ods { sheet: Option<String> },
}

/// A polling source backed by one file on disk.
#[derive(Clone, Debug)]
pub struct FilePollingSource {
    id: String,
    path: PathBuf,
    format: FileFormat,
    interval: Duration,
    state: SourceState,
    sequence: u64,
    /// The length and modification time of the last parsed contents.
    last: Option<(SystemTime, u64)>,
}

impl FilePollingSource {
    /// A stopped source for `path`, polled at `interval`.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        path: impl Into<PathBuf>,
        format: FileFormat,
        interval: Duration,
    ) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
            format,
            interval,
            state: SourceState::Stopped,
            sequence: 0,
            last: None,
        }
    }

    /// The file being read.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Forgets the last read so the next poll parses the file again even if
    /// it did not change.
    pub fn invalidate(&mut self) {
        self.last = None;
    }

    fn parse(&self, bytes: &[u8]) -> Result<DataValue, SourceError> {
        match &self.format {
            FileFormat::Csv(options) => {
                let adapter = parse_csv(bytes, options)
                    .map_err(|error| SourceError::Adapter(error.to_string()))?;
                adapter
                    .representation()
                    .map_err(|error| SourceError::Adapter(error.to_string()))
            }
            FileFormat::Json(limits) => {
                parse_json(bytes, limits).map_err(|error| SourceError::Adapter(error.to_string()))
            }
            FileFormat::Xlsx { sheet } => {
                let adapter = match sheet.as_deref() {
                    Some(name) => parse_xlsx_sheet(bytes, name),
                    None => parse_xlsx(bytes),
                }
                .map_err(|error| SourceError::Adapter(error.to_string()))?;
                adapter
                    .representation()
                    .map_err(|error| SourceError::Adapter(error.to_string()))
            }
            FileFormat::Ods { sheet } => {
                let adapter = match sheet.as_deref() {
                    Some(name) => parse_ods_sheet(bytes, name),
                    None => parse_ods(bytes),
                }
                .map_err(|error| SourceError::Adapter(error.to_string()))?;
                adapter
                    .representation()
                    .map_err(|error| SourceError::Adapter(error.to_string()))
            }
        }
    }
}

impl DataSource for FilePollingSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn state(&self) -> SourceState {
        self.state
    }

    fn start(&mut self) -> Result<(), SourceError> {
        if self.state == SourceState::Running {
            return Err(SourceError::AlreadyRunning);
        }
        self.state = SourceState::Running;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), SourceError> {
        if self.state == SourceState::Stopped {
            return Err(SourceError::NotRunning);
        }
        self.state = SourceState::Stopped;
        Ok(())
    }
}

impl PollingSource for FilePollingSource {
    fn poll_interval(&self) -> Duration {
        self.interval
    }

    fn poll(&mut self) -> Result<Option<SourceEvent>, SourceError> {
        if self.state != SourceState::Running {
            return Err(SourceError::NotRunning);
        }
        let metadata = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SourceError::Adapter(error.to_string())),
        };
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let length = metadata.len();
        if self.last == Some((modified, length)) {
            return Ok(None);
        }
        if length > MAX_FILE_SOURCE_BYTES as u64 {
            return Err(SourceError::Adapter(format!(
                "file is larger than {MAX_FILE_SOURCE_BYTES} bytes"
            )));
        }
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SourceError::Adapter(error.to_string())),
        };
        if bytes.len() > MAX_FILE_SOURCE_BYTES {
            return Err(SourceError::Adapter(format!(
                "file is larger than {MAX_FILE_SOURCE_BYTES} bytes"
            )));
        }
        let value = self.parse(&bytes)?;
        self.last = Some((modified, length));
        let sequence = self.sequence;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(SourceError::SequenceExhausted)?;
        Ok(Some(SourceEvent { sequence, value }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("fm-data-file-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        directory.join("source.dat")
    }

    #[test]
    fn a_json_file_emits_only_when_it_changes() {
        let path = temp_path("json");
        std::fs::write(&path, br#"{"name": "Ada", "number": 7}"#).unwrap();
        let mut source = FilePollingSource::new(
            "json",
            &path,
            FileFormat::Json(ParseLimits::default()),
            Duration::from_secs(1),
        );
        assert_eq!(source.poll(), Err(SourceError::NotRunning));
        source.start().unwrap();
        let first = source.poll().unwrap().unwrap();
        assert_eq!(first.sequence, 0);
        assert_eq!(
            first.value,
            DataValue::Object(
                [
                    ("name".to_owned(), DataValue::String("Ada".to_owned())),
                    ("number".to_owned(), DataValue::Integer(7)),
                ]
                .into_iter()
                .collect()
            )
        );
        // Unchanged contents do not emit again.
        assert_eq!(source.poll().unwrap(), None);
        // A change emits with the next sequence number.
        std::fs::write(&path, br#"{"number": 8}"#).unwrap();
        let second = source.poll().unwrap().unwrap();
        assert_eq!(second.sequence, 1);
        // invalidate() forces a re-read of unchanged bytes.
        source.invalidate();
        assert!(source.poll().unwrap().is_some());
        source.stop().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_csv_file_becomes_rows_of_objects() {
        let path = temp_path("csv");
        std::fs::write(&path, b"name,score\nAda,7\nGrace,9\n").unwrap();
        let mut source = FilePollingSource::new(
            "csv",
            &path,
            FileFormat::Csv(CsvOptions::default()),
            Duration::from_secs(1),
        );
        source.start().unwrap();
        let event = source.poll().unwrap().unwrap();
        let DataValue::List(rows) = event.value else {
            panic!("expected rows");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1],
            DataValue::Object(
                [
                    ("name".to_owned(), DataValue::String("Grace".to_owned())),
                    ("score".to_owned(), DataValue::String("9".to_owned())),
                ]
                .into_iter()
                .collect()
            )
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn package(parts: &[(&str, &str)]) -> Vec<u8> {
        use std::io::{Cursor, Write};

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, contents) in parts {
            writer
                .start_file(
                    (*name).to_owned(),
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn workbook() -> Vec<u8> {
        package(&[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/><sheet name="Other" sheetId="2" r:id="rId2"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="worksheet" Target="/xl/worksheets/sheet2.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row><c r="A1"><v>Name</v></c><c r="B1"><v>Score</v></c></row><row><c r="A2"><v>Ada</v></c><c r="B2"><v>7</v></c></row></sheetData></worksheet>"#,
            ),
            (
                "xl/worksheets/sheet2.xml",
                r#"<worksheet><sheetData><row><c r="A1"><v>Label</v></c></row><row><c r="A2"><v>Other</v></c></row></sheetData></worksheet>"#,
            ),
        ])
    }

    #[test]
    fn an_xlsx_file_polls_a_worksheet_and_emits_only_on_change() {
        let path = temp_path("xlsx");
        std::fs::write(&path, workbook()).unwrap();
        let mut source = FilePollingSource::new(
            "xlsx",
            &path,
            FileFormat::Xlsx {
                sheet: Some("Data".to_owned()),
            },
            Duration::from_secs(1),
        );
        source.start().unwrap();
        let first = source.poll().unwrap().unwrap();
        assert_eq!(first.sequence, 0);
        let DataValue::List(rows) = first.value else {
            panic!("an XLSX source emits rows");
        };
        assert_eq!(rows.len(), 1);
        let DataValue::Object(row) = &rows[0] else {
            panic!("an XLSX row is an object");
        };
        assert_eq!(row["Name"], DataValue::from("Ada"));
        assert_eq!(row["Score"], DataValue::from("7"));
        assert_eq!(source.poll().unwrap(), None, "an unchanged file is skipped");

        let mut named = FilePollingSource::new(
            "xlsx-named",
            &path,
            FileFormat::Xlsx {
                sheet: Some("Other".to_owned()),
            },
            Duration::from_secs(1),
        );
        named.start().unwrap();
        let event = named.poll().unwrap().unwrap();
        assert_eq!(
            event.value,
            DataValue::List(vec![DataValue::Object(
                [("Label".to_owned(), DataValue::from("Other"))]
                    .into_iter()
                    .collect()
            )])
        );
    }

    #[test]
    fn a_missing_xlsx_sheet_is_an_adapter_error() {
        let path = temp_path("xlsx-missing");
        std::fs::write(&path, workbook()).unwrap();
        let mut source = FilePollingSource::new(
            "xlsx-missing",
            &path,
            FileFormat::Xlsx {
                sheet: Some("Nope".to_owned()),
            },
            Duration::from_secs(1),
        );
        source.start().unwrap();
        assert!(matches!(
            source.poll(),
            Err(SourceError::Adapter(message)) if message.contains("no worksheet named Nope")
        ));
    }

    fn ods_package() -> Vec<u8> {
        package(&[(
            "content.xml",
            r#"<office:document-content xmlns:office="o" xmlns:table="t" xmlns:text="x"><office:body><office:spreadsheet><table:table table:name="Data"><table:table-row><table:table-cell office:value-type="string"><text:p>Name</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>Score</text:p></table:table-cell></table:table-row><table:table-row><table:table-cell office:value-type="string"><text:p>Ada</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="7"/></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#,
        )])
    }

    #[test]
    fn an_ods_file_polls_a_named_table() {
        let path = temp_path("ods");
        std::fs::write(&path, ods_package()).unwrap();
        let mut source = FilePollingSource::new(
            "ods",
            &path,
            FileFormat::Ods {
                sheet: Some("Data".to_owned()),
            },
            Duration::from_secs(1),
        );
        source.start().unwrap();
        let event = source.poll().unwrap().unwrap();
        assert_eq!(
            event.value,
            DataValue::List(vec![DataValue::Object(
                [
                    ("Name".to_owned(), DataValue::from("Ada")),
                    ("Score".to_owned(), DataValue::from("7")),
                ]
                .into_iter()
                .collect()
            )])
        );
        assert_eq!(source.poll().unwrap(), None);
    }

    #[test]
    fn a_missing_file_is_silent_and_bad_bytes_are_an_adapter_error() {
        let path = temp_path("missing");
        std::fs::remove_file(&path).ok();
        let mut source = FilePollingSource::new(
            "missing",
            &path,
            FileFormat::Json(ParseLimits::default()),
            Duration::from_secs(1),
        );
        source.start().unwrap();
        assert_eq!(source.poll().unwrap(), None);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(source.poll(), Err(SourceError::Adapter(_))));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
