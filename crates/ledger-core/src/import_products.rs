//! Spec 06 §3 — Products onboarding importer. v1 is template-only, fixed
//! column order: name, kind (product/service), sku, sale_price, is_vatable
//! (yes/no), qty_on_hand, unit_cost. The first row is always a header.
//!
//! Deviation from the spec's column list, noted here rather than silently:
//! the spec table lists both a plain "cost" column and a "unit cost†"
//! (inventory-only) column, but `products` has no general cost field to put
//! a standalone "cost" in — only the opening-stock valuation needs one. v1
//! collapses these into a single `unit_cost` column, used only when
//! inventory is tracked and a quantity on hand is given.
//!
//! Anti-double-count: unlike invoices/bills (Spec 06 §3.1's guard against the
//! wizard's lump AR/AP line), products have no equivalent lump entry to guard
//! against — the wizard creates real `products` rows directly. The ordinary
//! name/SKU dedup below (warn + skip on an existing match) already prevents
//! re-creating a product the wizard made and double-posting its opening stock.

use crate::engine::EngineError;
use crate::ids::{new_id, now_iso};
use crate::money::round_ratio;
use crate::posting::{post_entry_in, LineSpec};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

type R<T> = Result<T, EngineError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    Ready,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct ProductImportRow {
    pub row_num: usize,
    pub name: String,
    pub kind: String, // "product" | "service"
    pub sku: Option<String>,
    pub sale_price_kobo: i64,
    pub is_vatable: bool,
    pub qty_on_hand_milli: i64,
    pub unit_cost_kobo: i64,
    pub status: RowStatus,
    pub message: Option<String>,
}

/// Stages 2–4, read-only. `inventory_enabled` comes from the company row —
/// when off, qty/cost columns are accepted but simply ignored (no opening
/// stock movement is ever created for a company that doesn't track it).
pub fn preview_products(
    conn: &Connection, company_id: &str, inventory_enabled: bool, rows: &[Vec<String>],
) -> Vec<ProductImportRow> {
    rows.iter()
        .enumerate()
        .skip(1)
        .map(|(i, row)| {
            let row_num = i + 1;
            let cell = |idx: usize| row.get(idx).map(|s| s.trim().to_string()).unwrap_or_default();
            let name = cell(0);
            let kind_raw = cell(1).to_lowercase();
            let sku = opt(cell(2));
            let sale_price_raw = cell(3);
            let vatable_raw = cell(4).to_lowercase();
            let qty_raw = cell(5);
            let cost_raw = cell(6);

            if name.is_empty() {
                return error_row(row_num, name, sku, "Name is required".into());
            }
            let kind = if kind_raw.is_empty() {
                "product".to_string()
            } else {
                match kind_raw.as_str() {
                    "product" | "service" => kind_raw,
                    other => {
                        return error_row(
                            row_num, name, sku,
                            format!("Unrecognized kind \"{other}\" — must be product or service"),
                        )
                    }
                }
            };
            let sale_price_kobo = if sale_price_raw.is_empty() {
                0
            } else {
                match crate::csv_util::parse_kobo_cell(&sale_price_raw) {
                    Some(k) => k,
                    None => {
                        return error_row(row_num, name, sku, format!("Sale price \"{sale_price_raw}\" is not a number"))
                    }
                }
            };
            let is_vatable = match vatable_raw.as_str() {
                "" | "yes" | "y" | "true" | "1" => true,
                "no" | "n" | "false" | "0" => false,
                other => {
                    return error_row(row_num, name, sku, format!("\"{other}\" isn't yes or no for VAT"))
                }
            };
            let qty_on_hand_milli = if qty_raw.is_empty() {
                0
            } else {
                match crate::csv_util::parse_qty_milli_cell(&qty_raw) {
                    Some(q) => q,
                    None => return error_row(row_num, name, sku, format!("Quantity \"{qty_raw}\" is not a number")),
                }
            };
            let unit_cost_kobo = if cost_raw.is_empty() {
                0
            } else {
                match crate::csv_util::parse_kobo_cell(&cost_raw) {
                    Some(k) => k,
                    None => return error_row(row_num, name, sku, format!("Unit cost \"{cost_raw}\" is not a number")),
                }
            };
            if inventory_enabled && qty_on_hand_milli > 0 && unit_cost_kobo <= 0 {
                return error_row(
                    row_num, name, sku,
                    "Quantity on hand needs a unit cost greater than zero to value the opening stock".into(),
                );
            }

            let dup = conn
                .query_row(
                    "SELECT 1 FROM products WHERE company_id = ?1 AND (lower(trim(name)) = lower(trim(?2))
                     OR (?3 IS NOT NULL AND trim(?3) != '' AND lower(trim(sku)) = lower(trim(?3)))) LIMIT 1",
                    params![company_id, name, sku],
                    |_| Ok(()),
                )
                .optional()
                .ok()
                .flatten()
                .is_some();

            if dup {
                ProductImportRow {
                    row_num, name, kind, sku, sale_price_kobo, is_vatable, qty_on_hand_milli, unit_cost_kobo,
                    status: RowStatus::Warning,
                    message: Some("A product with this name or SKU already exists — will be skipped".into()),
                }
            } else {
                ProductImportRow {
                    row_num, name, kind, sku, sale_price_kobo, is_vatable, qty_on_hand_milli, unit_cost_kobo,
                    status: RowStatus::Ready,
                    message: None,
                }
            }
        })
        .collect()
}

fn opt(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn error_row(row_num: usize, name: String, sku: Option<String>, message: String) -> ProductImportRow {
    ProductImportRow {
        row_num, name, kind: "product".into(), sku, sale_price_kobo: 0, is_vatable: true,
        qty_on_hand_milli: 0, unit_cost_kobo: 0,
        status: RowStatus::Error,
        message: Some(message),
    }
}

pub struct ImportBatchResult {
    pub batch_id: String,
    pub rows_total: usize,
    pub rows_ok: usize,
    pub rows_error: usize,
    pub exceptions: Vec<ProductImportRow>,
}

/// Stage 5 (Commit). Writes `Ready` rows plus, for inventory-tracked product
/// rows with a quantity on hand, an `inventory_movements` row (kind
/// `'opening'`) and a Dr Inventory / Cr 3000 OPENING_BALANCE_EQUITY journal
/// entry — mirroring the purchase-posting shape in `post_bill`, valued at the
/// entered unit cost as the WAC baseline (Spec 06 §3 table).
pub fn commit_products(
    conn: &mut Connection, company_id: &str, filename: &str, posting_date: &str,
    inventory_enabled: bool, rows: Vec<ProductImportRow>, created_by: Option<&str>,
) -> R<ImportBatchResult> {
    let rows_total = rows.len();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut rows_ok = 0usize;
    let mut exceptions = Vec::new();
    for row in rows {
        if row.status != RowStatus::Ready {
            exceptions.push(row);
            continue;
        }
        let track_inventory = inventory_enabled && row.kind == "product";
        let product_id = new_id();
        tx.execute(
            "INSERT INTO products (id, company_id, kind, name, sku, sale_price_kobo, is_vatable,
                                    track_inventory, is_active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)",
            params![
                product_id, company_id, row.kind, row.name, row.sku, row.sale_price_kobo,
                row.is_vatable as i64, track_inventory as i64
            ],
        )?;

        if track_inventory && row.qty_on_hand_milli > 0 {
            let total_cost = round_ratio(row.qty_on_hand_milli as i128 * row.unit_cost_kobo as i128, 1000);
            let inventory_acct = crate::engine::sys(&tx, company_id, "INVENTORY")?;
            let obe = crate::engine::sys(&tx, company_id, "OPENING_BALANCE_EQUITY")?;
            let entry_id = post_entry_in(
                &tx, company_id, posting_date,
                &format!("Opening stock — {} (imported)", row.name),
                "opening_balance", Some(&product_id), created_by,
                &[LineSpec::new(&inventory_acct, total_cost), LineSpec::new(&obe, -total_cost)],
            )?;
            tx.execute(
                "INSERT INTO inventory_movements (id, company_id, product_id, movement_date, kind,
                                                  quantity_milli, unit_cost_kobo, total_cost_kobo,
                                                  journal_entry_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'opening', ?5, ?6, ?7, ?8, ?9)",
                params![
                    new_id(), company_id, product_id, posting_date, row.qty_on_hand_milli,
                    row.unit_cost_kobo, total_cost, entry_id, now_iso()
                ],
            )?;
        }
        rows_ok += 1;
    }
    let batch_id = new_id();
    let rows_error = rows_total - rows_ok;
    tx.execute(
        "INSERT INTO import_batches (id, company_id, kind, filename, rows_total, rows_ok, rows_error, created_by, created_at)
         VALUES (?1, ?2, 'products', ?3, ?4, ?5, ?6, ?7, ?8)",
        params![batch_id, company_id, filename, rows_total as i64, rows_ok as i64, rows_error as i64, created_by, now_iso()],
    )?;
    tx.commit()?;
    Ok(ImportBatchResult { batch_id, rows_total, rows_ok, rows_error, exceptions })
}
