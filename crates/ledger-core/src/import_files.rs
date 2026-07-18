//! Turns an uploaded onboarding/bulk-import file (xlsx/xls/csv/txt) into rows
//! of cell strings — Spec 06 §2 stage 1 (Parse), shared by every importer.

use crate::csv_util::parse_csv_rows;
use calamine::{open_workbook_from_rs, Reader, Xlsx};
use std::io::Cursor;

/// Dispatches on the filename's extension: `.csv`/`.txt` are read as UTF-8
/// text (the same parser bank-statement import uses); anything else is read
/// as an xlsx workbook via `calamine`. Returns a plain string error — this is
/// a low-level parse step, not an `EngineError`-producing operation.
pub fn rows_from_upload(filename: &str, bytes: &[u8]) -> Result<Vec<Vec<String>>, String> {
    let lower = filename.to_lowercase();
    if lower.ends_with(".csv") || lower.ends_with(".txt") {
        let text = String::from_utf8_lossy(bytes);
        Ok(parse_csv_rows(&text))
    } else {
        read_xlsx_rows(bytes)
    }
}

fn read_xlsx_rows(bytes: &[u8]) -> Result<Vec<Vec<String>>, String> {
    let cursor = Cursor::new(bytes);
    let mut wb = open_workbook_from_rs::<Xlsx<_>, _>(cursor).map_err(|e| e.to_string())?;
    let range = wb
        .worksheet_range_at(0)
        .ok_or_else(|| "the workbook has no sheets".to_string())?
        .map_err(|e| e.to_string())?;
    Ok(range
        .rows()
        .map(|row| row.iter().map(|cell| cell.to_string()).collect())
        .collect())
}
