//! LedgerOne accounting core (Spec 01).
//!
//! Everything financial flows through this crate. The UI (Tauri commands) never
//! touches SQLite directly — Spec 01 P2. The database itself enforces the core
//! invariants via triggers (Spec 01 §4); this crate's posting functions are the
//! friendly first line of validation, not the only line.

pub use rusqlite; // single rusqlite version for all downstream crates

pub mod db;
pub mod engine;
pub mod recon;
pub mod ids;
pub mod money;
pub mod posting;
pub mod seed;

pub use db::open;
pub use engine::{EngineError, PostCtx};
pub use posting::{post_entry, post_entry_in, LineSpec, PostError};
