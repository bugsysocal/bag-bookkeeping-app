//! Minimal quoted-CSV line splitting, shared by bank-statement import (Spec 04
//! §6) and the onboarding/bulk importers (Spec 06 §2) — one implementation,
//! not two copies drifting apart.

/// Splits one CSV line respecting double-quoted fields (with `""` as an
/// escaped quote inside a quoted field). Nigerian bank/Excel exports are
/// simple enough that this covers them without a full CSV-grammar crate.
pub fn split_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_q && chars.peek() == Some(&'"') => { cur.push('"'); chars.next(); }
            '"' => in_q = !in_q,
            ',' if !in_q => { out.push(cur.trim().to_string()); cur = String::new(); }
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

/// Parse a whole CSV text into rows of cells, skipping blank lines.
pub fn parse_csv_rows(text: &str) -> Vec<Vec<String>> {
    text.lines().filter(|l| !l.trim().is_empty()).map(split_csv_line).collect()
}

/// Clean a money-ish cell ("₦1,500,000.00", " 1 500 000 ") down to a
/// parseable number string. Spec 06 §2: amounts must parse as numbers —
/// anything left over after stripping ₦/commas/spaces is a row error.
pub fn clean_amount_str(raw: &str) -> String {
    raw.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect()
}

/// Parses a money cell straight to integer kobo. `None` means the cell
/// wasn't blank but also didn't parse as a number — the caller turns that
/// into a row error; a blank cell is the caller's own default (usually 0),
/// not this function's concern.
pub fn parse_kobo_cell(raw: &str) -> Option<i64> {
    let cleaned = clean_amount_str(raw);
    if cleaned.is_empty() {
        return None;
    }
    let naira: f64 = cleaned.parse().ok()?;
    Some((naira * 100.0).round() as i64)
}

/// Parses a quantity cell into milli-units (×1000, matching `quantity_milli`
/// throughout the schema — e.g. Spec 01's invoice/bill lines).
pub fn parse_qty_milli_cell(raw: &str) -> Option<i64> {
    let cleaned = clean_amount_str(raw);
    if cleaned.is_empty() {
        return None;
    }
    let qty: f64 = cleaned.parse().ok()?;
    Some((qty * 1000.0).round() as i64)
}

/// Parses a date cell to ISO (`YYYY-MM-DD`) — Spec 06 §2: "DD/MM/YYYY first,
/// ISO fallback." A leading 4-digit component is taken as an already-ISO year;
/// otherwise the Nigerian DD/MM/YYYY reading is used. Two-digit years are
/// assumed 2000s.
pub fn parse_date_cell(raw: &str) -> Option<String> {
    let seps: &[char] = &['/', '-', '.', ' '];
    let parts: Vec<&str> = raw.trim().split(seps).filter(|p| !p.is_empty()).collect();
    if parts.len() != 3 {
        return None;
    }
    let nums: Vec<i64> = parts.iter().map(|p| p.parse().ok()).collect::<Option<Vec<_>>>()?;
    let (y, m, d) = if parts[0].len() == 4 {
        (nums[0], nums[1], nums[2])
    } else {
        let mut y = nums[2];
        if y < 100 {
            y += 2000;
        }
        (y, nums[1], nums[0])
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(format!("{y:04}-{m:02}-{d:02}"))
}
