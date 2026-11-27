//! CSV telemetry imports, independent of UART transport and the GUI.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use wavecraft_core::CaptureError;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum CsvDelimiter {
    #[default]
    Auto,
    Comma,
    Semicolon,
    Tab,
}

impl CsvDelimiter {
    fn byte(self, bytes: &[u8]) -> u8 {
        match self {
            Self::Comma => b',',
            Self::Semicolon => b';',
            Self::Tab => b'\t',
            Self::Auto => {
                let candidates = *b",;\t";
                let mut counts = [0_usize; 3];
                let mut quoted = false;
                let mut started = false;
                for &byte in bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes) {
                    if byte == b'"' {
                        quoted = !quoted;
                    }
                    if !quoted {
                        if matches!(byte, b'\r' | b'\n') {
                            if started {
                                break;
                            }
                            continue;
                        }
                        for (index, candidate) in candidates.iter().enumerate() {
                            if byte == *candidate {
                                counts[index] += 1;
                            }
                        }
                    }
                    started = true;
                }
                let mut best = 0;
                for index in 1..candidates.len() {
                    if counts[index] > counts[best] {
                        best = index;
                    }
                }
                candidates[best]
            }
        }
    }
}

pub struct CsvReader<'a> {
    reader: ::csv::Reader<&'a [u8]>,
    columns: Vec<String>,
}

impl<'a> CsvReader<'a> {
    pub fn new(
        bytes: &'a [u8],
        delimiter: CsvDelimiter,
        has_headers: bool,
    ) -> Result<Self, CaptureError> {
        let mut reader = ::csv::ReaderBuilder::new()
            .delimiter(delimiter.byte(bytes))
            .has_headers(has_headers)
            .trim(::csv::Trim::All)
            .from_reader(bytes);
        let headers = reader.headers().map_err(csv_error)?;
        if headers.is_empty() {
            return Err("CSV file has no columns".into());
        }
        if headers.len() > 128 {
            return Err("CSV imports support at most 128 columns".into());
        }
        let columns: Vec<_> = headers
            .iter()
            .enumerate()
            .map(|(index, name)| {
                if !has_headers || name.is_empty() {
                    format!("Column {}", index + 1)
                } else {
                    name.to_owned()
                }
            })
            .collect();
        let mut unique = HashSet::new();
        if columns.iter().any(|name| name.len() > 256) {
            return Err("CSV column names are limited to 256 UTF-8 bytes".into());
        }
        if columns.iter().any(|name| !unique.insert(name)) {
            return Err("CSV column names must be unique; disable the header option for files without column names".into());
        }
        Ok(Self { reader, columns })
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    pub fn records(
        self,
        fields: &[String],
    ) -> Result<impl Iterator<Item = Result<Vec<Option<f64>>, CaptureError>> + 'a, CaptureError>
    {
        let indices: Vec<_> = fields
            .iter()
            .map(|name| {
                self.columns
                    .iter()
                    .position(|column| column == name)
                    .ok_or_else(|| CaptureError::new(format!("CSV column '{name}' does not exist")))
            })
            .collect::<Result<_, _>>()?;
        Ok(self.reader.into_records().map(move |record| {
            let record = record.map_err(csv_error)?;
            Ok(indices
                .iter()
                .map(|&index| {
                    record
                        .get(index)
                        .and_then(|value| value.trim().parse::<f64>().ok())
                        .filter(|value| value.is_finite())
                })
                .collect())
        }))
    }
}

fn csv_error(error: ::csv::Error) -> CaptureError {
    CaptureError::new(format!("CSV: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_delimiters_and_selects_named_columns_with_bom_quotes_and_missing_values() {
        for delimiter in [",", ";", "\t"] {
            let bytes = format!("\u{feff}time{delimiter}\"value, sensor\"{delimiter}unused\r\n0{delimiter}1.5{delimiter}\"a\"\"b\"\r\n1{delimiter}{delimiter}\"two\nlines\"\r\n2{delimiter}-2.5e1{delimiter}x\r\n");
            let reader = CsvReader::new(bytes.as_bytes(), CsvDelimiter::Auto, true).unwrap();
            assert_eq!(reader.columns(), ["time", "value, sensor", "unused"]);
            let records: Vec<_> = reader
                .records(&["value, sensor".into(), "time".into()])
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(
                records,
                vec![
                    vec![Some(1.5), Some(0.0)],
                    vec![None, Some(1.0)],
                    vec![Some(-25.0), Some(2.0)]
                ]
            );
        }
    }

    #[test]
    fn headerless_files_keep_the_first_record_and_reject_ambiguous_or_invalid_input() {
        assert_eq!(
            CsvReader::new(b"\r\na;b\r\n1;2\r\n", CsvDelimiter::Auto, true)
                .unwrap()
                .columns(),
            ["a", "b"]
        );
        let reader = CsvReader::new(b"0;42\n1;43\n", CsvDelimiter::Semicolon, false).unwrap();
        assert_eq!(reader.columns(), ["Column 1", "Column 2"]);
        let values: Vec<_> = reader
            .records(&["Column 2".into()])
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(values, vec![vec![Some(42.0)], vec![Some(43.0)]]);
        assert!(CsvReader::new(b"a,a\n1,2\n", CsvDelimiter::Auto, true).is_err());
        assert!(CsvReader::new(b"", CsvDelimiter::Auto, true).is_err());
        let reader = CsvReader::new(b"a,b\n1\n2,3\n", CsvDelimiter::Auto, true).unwrap();
        let mut records = reader.records(&["a".into()]).unwrap();
        assert!(records.next().unwrap().is_err());
        assert_eq!(records.next().unwrap().unwrap(), vec![Some(2.0)]);
    }
}
