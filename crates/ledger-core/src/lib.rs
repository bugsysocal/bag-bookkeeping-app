//! LedgerOne accounting core (Spec 01).
//!
//! Everything financial flows through this crate. The UI (Tauri commands) never
//! touches SQLite directly — Spec 01 P2. The database itself enforces the core
//! invariants via triggers (Spec 01 §4); this crate's posting functions are the
//! friendly first line of validation, not the only line.

pub use rusqlite; // single rusqlite version for all downstream crates

pub mod auth;
pub mod backup;
pub mod compliance;
pub mod csv_util;
pub mod db;
pub mod engine;
pub mod export_xlsx;
pub mod import_contacts;
pub mod import_files;
pub mod import_open_bills;
pub mod import_open_invoices;
pub mod import_products;
pub mod recon;
pub mod reports;
pub mod ids;
pub mod money;
pub mod posting;
pub mod seed;

pub use db::open;
pub use engine::{EngineError, PostCtx};
pub use posting::{post_entry, post_entry_in, LineSpec, PostError};
