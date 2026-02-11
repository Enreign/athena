use rusqlite::Connection;
use std::path::Path;

use crate::error::{AthenaError, Result};

const MIGRATIONS: &[&str] = &[
    // v1: memories table + schema version tracking
    "CREATE TABLE IF NOT EXISTS memories (
        id TEXT PRIMARY KEY,
        category TEXT NOT NULL,
        content TEXT NOT NULL,
        active INTEGER NOT NULL DEFAULT 1,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    );
    CREATE TABLE IF NOT EXISTS schema_version (
        version INTEGER PRIMARY KEY
    );",
    // v2: embedding column for vector search
    "ALTER TABLE memories ADD COLUMN embedding BLOB;",
    // v3: FTS5 full-text search index (standalone, not external-content)
    "CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(content);
     INSERT INTO memories_fts(rowid, content) SELECT rowid, content FROM memories WHERE active = 1;",
    // v4: conversation history for multi-turn context
    "CREATE TABLE IF NOT EXISTS conversations (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_key TEXT NOT NULL,
        role TEXT NOT NULL,
        content TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    );
    CREATE INDEX IF NOT EXISTS idx_conversations_session ON conversations(session_key, created_at);",
];

pub fn init_db(path: &Path) -> Result<Connection> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AthenaError::Config(format!("Failed to create db directory: {}", e)))?;
    }

    let conn = Connection::open(path)?;

    // Enable WAL mode for better concurrency
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;

    run_migrations(&conn)?;

    Ok(conn)
}

fn current_version(conn: &Connection) -> i64 {
    // schema_version table might not exist yet
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

fn run_migrations(conn: &Connection) -> Result<()> {
    // First, ensure at least the base tables exist so we can query schema_version.
    // If this is a fresh DB, run migration 0 unconditionally.
    let has_schema_table: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !has_schema_table {
        // Fresh database — run the first migration to bootstrap
        conn.execute_batch(MIGRATIONS[0])?;
        conn.execute("INSERT INTO schema_version (version) VALUES (?1)", [1i64])?;
    }

    let current = current_version(conn);

    for (i, migration) in MIGRATIONS.iter().enumerate() {
        let version = (i + 1) as i64;
        if version <= current {
            continue;
        }
        tracing::info!("Running database migration v{}", version);
        conn.execute_batch(migration)?;
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [version],
        )?;
    }

    Ok(())
}
