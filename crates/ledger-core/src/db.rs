use rusqlite::Connection;

/// Migrations, in order. `PRAGMA user_version` tracks the last applied index + 1.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_license.sql"),
    include_str!("../migrations/0003_imported_paid.sql"),
    include_str!("../migrations/0004_compliance_ack.sql"),
];

/// Open (or create) a LedgerOne database and bring it to the current schema.
///
/// Every connection gets the Spec 01 §3 pragmas: foreign keys ON, WAL journal
/// (a no-op for in-memory databases, which tests use).
pub fn open(path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    let _ = conn.pragma_update(None, "journal_mode", "WAL"); // in-memory dbs report "memory"; fine
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let applied: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = (i + 1) as i64;
        if version > applied {
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", version)?;
        }
    }
    Ok(())
}
