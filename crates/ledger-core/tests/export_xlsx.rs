//! Spec 06 §5 xlsx export tests: round-trips the written bytes back through
//! `calamine` (the same library the importers read with) to prove the file
//! is genuinely valid Excel, not just "didn't panic" — checks the banner
//! row, the header block, the column headers, and that a money cell comes
//! back as the right naira value (kobo ÷ 100, since Excel stores numbers as
//! f64 regardless of the `₦#,##0.00` display format).

use calamine::{Data, Reader, Xlsx};
use ledger_core::export_xlsx::*;
use ledger_core::reports::{AgingBuckets, AgingRow, TrialBalanceRow};
use std::io::Cursor;

fn read_sheet(bytes: Vec<u8>) -> calamine::Range<Data> {
    let cursor = Cursor::new(bytes);
    let mut wb: Xlsx<_> = calamine::open_workbook_from_rs(cursor).unwrap();
    let name = wb.sheet_names()[0].clone();
    wb.worksheet_range(&name).unwrap()
}

fn header() -> ExportHeader<'static> {
    ExportHeader {
        company_name: "EdenOceans Test Co",
        report_title: "Aging Receivables",
        period_label: "As of 31/07/2026",
        generated_at: "2026-07-31T10:00:00",
    }
}

#[test]
fn export_aging_round_trips_through_calamine_with_correct_values() {
    let rows = vec![AgingRow {
        contact_id: "c1".into(),
        contact_name: "Zenith Traders".into(),
        buckets: AgingBuckets { current_kobo: 100_000_00, d1_30_kobo: 50_000_00, d31_60_kobo: 0, d61_90_kobo: 0, d90_plus_kobo: 0 },
        deposit_kobo: 20_000_00,
    }];
    let bytes = export_aging(&header(), &rows).unwrap();
    assert!(!bytes.is_empty());
    let sheet = read_sheet(bytes);

    // Banner row (row 0) carries the exact re-import-guard text.
    match sheet.get_value((0, 0)) {
        Some(Data::String(s)) => assert_eq!(s, REIMPORT_BANNER),
        other => panic!("expected banner string, got {other:?}"),
    }
    // Header block: company name (row 1), report title (row 2), period (row 3).
    assert_eq!(sheet.get_value((1, 0)), Some(&Data::String("EdenOceans Test Co".into())));
    assert_eq!(sheet.get_value((2, 0)), Some(&Data::String("Aging Receivables".into())));
    assert_eq!(sheet.get_value((3, 0)), Some(&Data::String("As of 31/07/2026".into())));

    // Column headers on row 6 (0-indexed).
    assert_eq!(sheet.get_value((6, 0)), Some(&Data::String("Contact".into())));
    assert_eq!(sheet.get_value((6, 1)), Some(&Data::String("Current".into())));

    // First data row, row 7: contact name + current-bucket amount in naira.
    assert_eq!(sheet.get_value((7, 0)), Some(&Data::String("Zenith Traders".into())));
    match sheet.get_value((7, 1)) {
        Some(Data::Float(f)) => assert!((f - 100_000.0).abs() < 0.001, "expected ₦100,000, got {f}"),
        other => panic!("expected a numeric current-bucket cell, got {other:?}"),
    }
    match sheet.get_value((7, 7)) {
        Some(Data::Float(f)) => assert!((f - 20_000.0).abs() < 0.001, "expected ₦20,000 deposit, got {f}"),
        other => panic!("expected a numeric deposit cell, got {other:?}"),
    }
}

#[test]
fn export_trial_balance_handles_multiple_rows_and_empty_rows() {
    let rows = vec![
        TrialBalanceRow { account_id: "a1".into(), code: "1100".into(), name: "Accounts Receivable".into(), class: "asset".into(), debit_kobo: 500_000_00, credit_kobo: 0 },
        TrialBalanceRow { account_id: "a2".into(), code: "3000".into(), name: "Opening Balance Equity".into(), class: "equity".into(), debit_kobo: 0, credit_kobo: 500_000_00 },
    ];
    let h = ExportHeader { report_title: "Trial Balance", ..header() };
    let bytes = export_trial_balance(&h, &rows).unwrap();
    let sheet = read_sheet(bytes);
    assert_eq!(sheet.get_value((6, 0)), Some(&Data::String("Code".into())));
    assert_eq!(sheet.get_value((7, 0)), Some(&Data::String("1100".into())));
    match sheet.get_value((7, 3)) {
        Some(Data::Float(f)) => assert!((f - 500_000.0).abs() < 0.001, "expected ₦500,000, got {f}"),
        other => panic!("expected numeric debit cell, got {other:?}"),
    }

    // An empty report still produces a valid, readable workbook (no data rows).
    let empty_bytes = export_trial_balance(&h, &[]).unwrap();
    let empty_sheet = read_sheet(empty_bytes);
    assert_eq!(empty_sheet.get_value((6, 0)), Some(&Data::String("Code".into())));
    assert_eq!(empty_sheet.get_value((7, 0)), None, "no data rows below the header");
}
