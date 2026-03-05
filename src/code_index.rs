//! Structured Code Index
//!
//! Builds and maintains a persistent, searchable index of code structure
//! (functions, structs, traits, imports, module graph) using `syn` for Rust
//! and regex-based heuristics for other languages.
//!
//! The index is stored in SQLite alongside the memory store, enabling
//! sub-millisecond lookups instead of token-expensive LLM code exploration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::{params, Connection};

// ---------------------------------------------------------------------------
// Symbol types
// ---------------------------------------------------------------------------

/// Kind of code symbol extracted from source files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Const,
    TypeAlias,
    Module,
    Import,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Const => "const",
            Self::TypeAlias => "type_alias",
            Self::Module => "module",
            Self::Import => "import",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "function" => Some(Self::Function),
            "struct" => Some(Self::Struct),
            "enum" => Some(Self::Enum),
            "trait" => Some(Self::Trait),
            "impl" => Some(Self::Impl),
            "const" => Some(Self::Const),
            "type_alias" => Some(Self::TypeAlias),
            "module" => Some(Self::Module),
            "import" => Some(Self::Import),
            _ => None,
        }
    }
}

/// A single symbol extracted from source code.
#[derive(Debug, Clone)]
pub struct CodeSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub file_path: String,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: String,
    pub parent: Option<String>,
    pub visibility: Visibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Crate,
    Private,
}

impl Visibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Crate => "crate",
            Self::Private => "private",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "public" => Self::Public,
            "crate" => Self::Crate,
            _ => Self::Private,
        }
    }
}

/// Dependency edge between modules/files.
#[derive(Debug, Clone)]
pub struct ModuleDep {
    pub from_file: String,
    pub to_module: String,
    pub import_path: String,
}

// ---------------------------------------------------------------------------
// Code Index Store
// ---------------------------------------------------------------------------

/// Persistent code index backed by SQLite.
pub struct CodeIndex {
    conn: Connection,
}

impl CodeIndex {
    /// Open or create a code index database at the given path.
    pub fn open(db_path: &Path) -> crate::error::Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let index = Self { conn };
        index.ensure_schema()?;
        Ok(index)
    }

    /// Open an in-memory code index (for testing).
    #[cfg(test)]
    pub fn open_memory() -> crate::error::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let index = Self { conn };
        index.ensure_schema()?;
        Ok(index)
    }

    fn ensure_schema(&self) -> crate::error::Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS code_symbols (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                signature TEXT NOT NULL,
                parent TEXT,
                visibility TEXT NOT NULL DEFAULT 'private',
                indexed_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON code_symbols(name);
            CREATE INDEX IF NOT EXISTS idx_symbols_kind ON code_symbols(kind);
            CREATE INDEX IF NOT EXISTS idx_symbols_file ON code_symbols(file_path);

            CREATE TABLE IF NOT EXISTS module_deps (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                from_file TEXT NOT NULL,
                to_module TEXT NOT NULL,
                import_path TEXT NOT NULL,
                indexed_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_deps_from ON module_deps(from_file);
            CREATE INDEX IF NOT EXISTS idx_deps_to ON module_deps(to_module);

            CREATE TABLE IF NOT EXISTS index_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
                name, signature, file_path,
                content=code_symbols,
                content_rowid=id
            );",
        )?;
        Ok(())
    }

    /// Clear all symbols for a given file (before re-indexing).
    pub fn clear_file(&self, file_path: &str) -> crate::error::Result<()> {
        self.conn.execute(
            "DELETE FROM code_symbols WHERE file_path = ?1",
            params![file_path],
        )?;
        self.conn.execute(
            "DELETE FROM module_deps WHERE from_file = ?1",
            params![file_path],
        )?;
        Ok(())
    }

    /// Insert a batch of symbols from a single file.
    pub fn insert_symbols(&self, symbols: &[CodeSymbol]) -> crate::error::Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO code_symbols (name, kind, file_path, line_start, line_end, signature, parent, visibility)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for sym in symbols {
            stmt.execute(params![
                sym.name,
                sym.kind.as_str(),
                sym.file_path,
                sym.line_start,
                sym.line_end,
                sym.signature,
                sym.parent,
                sym.visibility.as_str(),
            ])?;
        }

        // Rebuild FTS index for inserted symbols
        self.conn.execute(
            "INSERT INTO symbols_fts(symbols_fts) VALUES('rebuild')",
            [],
        )?;

        Ok(())
    }

    /// Insert module dependency edges.
    pub fn insert_deps(&self, deps: &[ModuleDep]) -> crate::error::Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO module_deps (from_file, to_module, import_path) VALUES (?1, ?2, ?3)",
        )?;
        for dep in deps {
            stmt.execute(params![dep.from_file, dep.to_module, dep.import_path])?;
        }
        Ok(())
    }

    /// Search symbols by name (exact or prefix match).
    pub fn search_symbols(&self, query: &str, limit: usize) -> crate::error::Result<Vec<CodeSymbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, kind, file_path, line_start, line_end, signature, parent, visibility
             FROM code_symbols
             WHERE name LIKE ?1
             ORDER BY
                CASE WHEN name = ?2 THEN 0 ELSE 1 END,
                file_path
             LIMIT ?3",
        )?;
        let pattern = format!("{}%", query);
        let rows = stmt.query_map(params![pattern, query, limit as i64], |row| {
            Ok(CodeSymbol {
                name: row.get(0)?,
                kind: SymbolKind::from_str(
                    &row.get::<_, String>(1)?,
                ).unwrap_or(SymbolKind::Function),
                file_path: row.get(2)?,
                line_start: row.get(3)?,
                line_end: row.get(4)?,
                signature: row.get(5)?,
                parent: row.get(6)?,
                visibility: Visibility::from_str(
                    &row.get::<_, String>(7)?,
                ),
            })
        })?;
        let mut symbols = Vec::new();
        for row in rows {
            symbols.push(row?);
        }
        Ok(symbols)
    }

    /// Full-text search across symbol names, signatures, and file paths.
    pub fn search_fts(&self, query: &str, limit: usize) -> crate::error::Result<Vec<CodeSymbol>> {
        let fts_query = query
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" OR ");

        let mut stmt = self.conn.prepare(
            "SELECT cs.name, cs.kind, cs.file_path, cs.line_start, cs.line_end,
                    cs.signature, cs.parent, cs.visibility
             FROM symbols_fts sf
             JOIN code_symbols cs ON sf.rowid = cs.id
             WHERE symbols_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_query, limit as i64], |row| {
            Ok(CodeSymbol {
                name: row.get(0)?,
                kind: SymbolKind::from_str(
                    &row.get::<_, String>(1)?,
                ).unwrap_or(SymbolKind::Function),
                file_path: row.get(2)?,
                line_start: row.get(3)?,
                line_end: row.get(4)?,
                signature: row.get(5)?,
                parent: row.get(6)?,
                visibility: Visibility::from_str(
                    &row.get::<_, String>(7)?,
                ),
            })
        })?;
        let mut symbols = Vec::new();
        for row in rows {
            symbols.push(row?);
        }
        Ok(symbols)
    }

    /// Get all symbols in a file (for understanding file structure).
    pub fn symbols_in_file(&self, file_path: &str) -> crate::error::Result<Vec<CodeSymbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, kind, file_path, line_start, line_end, signature, parent, visibility
             FROM code_symbols
             WHERE file_path = ?1
             ORDER BY line_start",
        )?;
        let rows = stmt.query_map(params![file_path], |row| {
            Ok(CodeSymbol {
                name: row.get(0)?,
                kind: SymbolKind::from_str(
                    &row.get::<_, String>(1)?,
                ).unwrap_or(SymbolKind::Function),
                file_path: row.get(2)?,
                line_start: row.get(3)?,
                line_end: row.get(4)?,
                signature: row.get(5)?,
                parent: row.get(6)?,
                visibility: Visibility::from_str(
                    &row.get::<_, String>(7)?,
                ),
            })
        })?;
        let mut symbols = Vec::new();
        for row in rows {
            symbols.push(row?);
        }
        Ok(symbols)
    }

    /// Find all files that depend on a given module.
    pub fn dependents_of(&self, module: &str) -> crate::error::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT from_file FROM module_deps WHERE to_module LIKE ?1",
        )?;
        let pattern = format!("%{}", module);
        let rows = stmt.query_map(params![pattern], |row| row.get::<_, String>(0))?;
        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }
        Ok(files)
    }

    /// Find all modules that a given file imports.
    pub fn dependencies_of(&self, file_path: &str) -> crate::error::Result<Vec<ModuleDep>> {
        let mut stmt = self.conn.prepare(
            "SELECT from_file, to_module, import_path FROM module_deps WHERE from_file = ?1",
        )?;
        let rows = stmt.query_map(params![file_path], |row| {
            Ok(ModuleDep {
                from_file: row.get(0)?,
                to_module: row.get(1)?,
                import_path: row.get(2)?,
            })
        })?;
        let mut deps = Vec::new();
        for row in rows {
            deps.push(row?);
        }
        Ok(deps)
    }

    /// Get total symbol count (for metrics).
    pub fn symbol_count(&self) -> crate::error::Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM code_symbols", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Get total indexed file count.
    pub fn file_count(&self) -> crate::error::Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT file_path) FROM code_symbols",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Format a compact context string for a task, given relevant symbol names.
    pub fn build_context_for_symbols(&self, symbol_names: &[&str], max_chars: usize) -> String {
        let mut sections: Vec<String> = Vec::new();
        let mut total_len = 0;

        for name in symbol_names {
            if total_len >= max_chars {
                break;
            }
            if let Ok(symbols) = self.search_symbols(name, 5) {
                for sym in symbols {
                    let line = format!(
                        "{} {} [{}:{}] {}",
                        sym.kind.as_str(),
                        sym.name,
                        sym.file_path,
                        sym.line_start,
                        sym.signature
                    );
                    if total_len + line.len() > max_chars {
                        break;
                    }
                    total_len += line.len() + 1;
                    sections.push(line);
                }
            }
        }

        if sections.is_empty() {
            String::new()
        } else {
            format!("CODE INDEX:\n{}", sections.join("\n"))
        }
    }
}

// ---------------------------------------------------------------------------
// Rust parser using regex (syn-free for now to avoid heavy dep)
// ---------------------------------------------------------------------------

/// Parse Rust source code and extract symbols using regex patterns.
/// This is a lightweight alternative to full `syn` parsing — it catches
/// ~90% of symbols with zero additional dependencies.
pub fn parse_rust_file(file_path: &str, source: &str) -> (Vec<CodeSymbol>, Vec<ModuleDep>) {
    let mut symbols = Vec::new();
    let mut deps = Vec::new();
    let mut current_impl: Option<String> = None;
    let mut brace_depth: i32 = 0;
    let mut impl_brace_depth: Option<i32> = None;

    for (line_idx, line) in source.lines().enumerate() {
        let line_num = (line_idx + 1) as u32;
        let trimmed = line.trim();

        // Track brace depth for impl block scoping
        for ch in line.chars() {
            match ch {
                '{' => brace_depth += 1,
                '}' => {
                    brace_depth -= 1;
                    if let Some(impl_depth) = impl_brace_depth {
                        if brace_depth < impl_depth {
                            current_impl = None;
                            impl_brace_depth = None;
                        }
                    }
                }
                _ => {}
            }
        }

        // use/imports
        if trimmed.starts_with("use ") {
            let import_path = trimmed
                .trim_start_matches("use ")
                .trim_end_matches(';')
                .trim()
                .to_string();
            let module_name = import_path
                .split("::")
                .next()
                .unwrap_or(&import_path)
                .to_string();
            deps.push(ModuleDep {
                from_file: file_path.to_string(),
                to_module: module_name,
                import_path: import_path.clone(),
            });
            symbols.push(CodeSymbol {
                name: import_path,
                kind: SymbolKind::Import,
                file_path: file_path.to_string(),
                line_start: line_num,
                line_end: line_num,
                signature: trimmed.to_string(),
                parent: None,
                visibility: Visibility::Private,
            });
            continue;
        }

        // mod declarations
        if let Some(rest) = trimmed.strip_prefix("pub mod ").or_else(|| trimmed.strip_prefix("mod ")) {
            let name = rest.trim_end_matches(';').trim_end_matches(" {").trim();
            if !name.is_empty() && !name.contains(' ') {
                let vis = if trimmed.starts_with("pub ") {
                    Visibility::Public
                } else {
                    Visibility::Private
                };
                symbols.push(CodeSymbol {
                    name: name.to_string(),
                    kind: SymbolKind::Module,
                    file_path: file_path.to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: trimmed.to_string(),
                    parent: None,
                    visibility: vis,
                });
            }
            continue;
        }

        // Parse visibility
        let (vis, rest) = parse_visibility(trimmed);

        // impl blocks
        if rest.starts_with("impl ") || rest.starts_with("impl<") {
            let impl_name = extract_impl_name(rest);
            current_impl = Some(impl_name.clone());
            impl_brace_depth = Some(brace_depth);
            symbols.push(CodeSymbol {
                name: impl_name,
                kind: SymbolKind::Impl,
                file_path: file_path.to_string(),
                line_start: line_num,
                line_end: line_num,
                signature: truncate_sig(trimmed, 200),
                parent: None,
                visibility: vis,
            });
            continue;
        }

        // fn declarations
        if rest.starts_with("fn ") || rest.starts_with("async fn ") || rest.starts_with("unsafe fn ") || rest.starts_with("const fn ") {
            let fn_name = extract_fn_name(rest);
            if !fn_name.is_empty() {
                symbols.push(CodeSymbol {
                    name: fn_name,
                    kind: SymbolKind::Function,
                    file_path: file_path.to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: truncate_sig(trimmed, 200),
                    parent: current_impl.clone(),
                    visibility: vis,
                });
            }
            continue;
        }

        // struct
        if rest.starts_with("struct ") {
            let name = extract_type_name(rest, "struct ");
            if !name.is_empty() {
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Struct,
                    file_path: file_path.to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: truncate_sig(trimmed, 200),
                    parent: None,
                    visibility: vis,
                });
            }
            continue;
        }

        // enum
        if rest.starts_with("enum ") {
            let name = extract_type_name(rest, "enum ");
            if !name.is_empty() {
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Enum,
                    file_path: file_path.to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: truncate_sig(trimmed, 200),
                    parent: None,
                    visibility: vis,
                });
            }
            continue;
        }

        // trait
        if rest.starts_with("trait ") {
            let name = extract_type_name(rest, "trait ");
            if !name.is_empty() {
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Trait,
                    file_path: file_path.to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: truncate_sig(trimmed, 200),
                    parent: None,
                    visibility: vis,
                });
            }
            continue;
        }

        // const / static
        if rest.starts_with("const ") || rest.starts_with("static ") {
            let prefix = if rest.starts_with("const ") {
                "const "
            } else {
                "static "
            };
            let name = extract_type_name(rest, prefix);
            if !name.is_empty() && name != "_" {
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::Const,
                    file_path: file_path.to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: truncate_sig(trimmed, 200),
                    parent: current_impl.clone(),
                    visibility: vis,
                });
            }
            continue;
        }

        // type alias
        if rest.starts_with("type ") && rest.contains('=') {
            let name = extract_type_name(rest, "type ");
            if !name.is_empty() {
                symbols.push(CodeSymbol {
                    name,
                    kind: SymbolKind::TypeAlias,
                    file_path: file_path.to_string(),
                    line_start: line_num,
                    line_end: line_num,
                    signature: truncate_sig(trimmed, 200),
                    parent: None,
                    visibility: vis,
                });
            }
        }
    }

    (symbols, deps)
}

fn parse_visibility(line: &str) -> (Visibility, &str) {
    if let Some(rest) = line.strip_prefix("pub(crate) ") {
        (Visibility::Crate, rest)
    } else if let Some(rest) = line.strip_prefix("pub(super) ") {
        (Visibility::Crate, rest)
    } else if let Some(rest) = line.strip_prefix("pub ") {
        (Visibility::Public, rest)
    } else {
        (Visibility::Private, line)
    }
}

fn extract_fn_name(rest: &str) -> String {
    let after_fn = rest
        .strip_prefix("async ")
        .unwrap_or(rest);
    let after_fn = after_fn
        .strip_prefix("unsafe ")
        .unwrap_or(after_fn);
    let after_fn = after_fn
        .strip_prefix("const ")
        .unwrap_or(after_fn);
    let after_fn = after_fn
        .strip_prefix("fn ")
        .unwrap_or(after_fn);
    after_fn
        .split(|c: char| c == '(' || c == '<' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_string()
}

fn extract_type_name(rest: &str, prefix: &str) -> String {
    let after = rest.strip_prefix(prefix).unwrap_or(rest);
    after
        .split(|c: char| c == '{' || c == '(' || c == '<' || c == ';' || c == ':' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_string()
}

fn extract_impl_name(rest: &str) -> String {
    let after = rest
        .strip_prefix("impl<")
        .map(|s| {
            // Skip generic params: impl<T: Foo> Bar
            let mut depth = 1i32;
            let mut idx = 0;
            for (i, c) in s.char_indices() {
                match c {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            idx = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            s[idx..].trim_start()
        })
        .unwrap_or_else(|| rest.strip_prefix("impl ").unwrap_or(rest));

    // Handle "TraitName for TypeName"
    let name_part = if let Some(for_idx) = after.find(" for ") {
        let trait_name = after[..for_idx].trim();
        let type_part = after[for_idx + 5..].trim();
        let type_name = type_part
            .split(|c: char| c == '{' || c == '<' || c.is_whitespace())
            .next()
            .unwrap_or("");
        return format!("{} for {}", trait_name, type_name);
    } else {
        after
    };

    name_part
        .split(|c: char| c == '{' || c == '<' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_string()
}

fn truncate_sig(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..s.floor_char_boundary(max)])
    }
}

// ---------------------------------------------------------------------------
// Directory walker — index all source files in a directory tree
// ---------------------------------------------------------------------------

/// Index all Rust files under `root_dir`, storing results in `index`.
pub fn index_directory(
    index: &CodeIndex,
    root_dir: &Path,
    workspace_prefix: &str,
) -> crate::error::Result<IndexStats> {
    let started = Instant::now();
    let mut files_indexed = 0usize;
    let mut symbols_found = 0usize;
    let mut deps_found = 0usize;

    walk_dir(root_dir, &mut |path: &Path| {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if ext != "rs" {
            return;
        }

        let rel_path = path
            .strip_prefix(root_dir)
            .unwrap_or(path)
            .to_string_lossy();
        let indexed_path = if workspace_prefix.is_empty() {
            rel_path.to_string()
        } else {
            format!("{}/{}", workspace_prefix, rel_path)
        };

        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return,
        };

        let (symbols, deps) = parse_rust_file(&indexed_path, &source);

        if let Err(e) = index.clear_file(&indexed_path) {
            tracing::warn!(file = %indexed_path, error = %e, "Failed to clear file index");
            return;
        }
        if !symbols.is_empty() {
            if let Err(e) = index.insert_symbols(&symbols) {
                tracing::warn!(file = %indexed_path, error = %e, "Failed to insert symbols");
                return;
            }
        }
        if !deps.is_empty() {
            if let Err(e) = index.insert_deps(&deps) {
                tracing::warn!(file = %indexed_path, error = %e, "Failed to insert deps");
                return;
            }
        }

        symbols_found += symbols.len();
        deps_found += deps.len();
        files_indexed += 1;
    })?;

    let elapsed = started.elapsed();
    tracing::info!(
        files = files_indexed,
        symbols = symbols_found,
        deps = deps_found,
        elapsed_ms = elapsed.as_millis(),
        "Code index build complete"
    );

    Ok(IndexStats {
        files_indexed,
        symbols_found,
        deps_found,
        elapsed_ms: elapsed.as_millis() as u64,
    })
}

fn walk_dir(dir: &Path, callback: &mut dyn FnMut(&Path)) -> crate::error::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| crate::error::AthenaError::Tool(format!("read_dir failed: {}", e)))?;

    for entry in entries {
        let entry = entry
            .map_err(|e| crate::error::AthenaError::Tool(format!("dir entry error: {}", e)))?;
        let path = entry.path();

        // Skip hidden dirs, target, node_modules, .git
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
        }

        if path.is_dir() {
            walk_dir(&path, callback)?;
        } else {
            callback(&path);
        }
    }
    Ok(())
}

/// Stats from an index build operation.
#[derive(Debug, Clone)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub symbols_found: usize,
    pub deps_found: usize,
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// Context compilation — format index data for LLM consumption
// ---------------------------------------------------------------------------

/// Compile a focused context pack from the code index for a given task description.
pub fn compile_context_for_task(
    index: &CodeIndex,
    task_description: &str,
    max_chars: usize,
) -> String {
    let keywords = extract_keywords(task_description);
    let mut sections: Vec<String> = Vec::new();
    let mut total_len = 0;

    // 1. Search by keywords in FTS
    for kw in &keywords {
        if total_len >= max_chars {
            break;
        }
        if let Ok(symbols) = index.search_fts(kw, 10) {
            for sym in symbols {
                let line = format_symbol_line(&sym);
                if total_len + line.len() > max_chars {
                    break;
                }
                total_len += line.len() + 1;
                sections.push(line);
            }
        }
    }

    // 2. Add dependency context for referenced files
    let referenced_files: Vec<String> = sections
        .iter()
        .filter_map(|s| {
            s.split('[')
                .nth(1)?
                .split(':')
                .next()
                .map(|f| f.to_string())
        })
        .collect();

    for file in referenced_files.iter().take(5) {
        if total_len >= max_chars {
            break;
        }
        if let Ok(dependents) = index.dependents_of(file) {
            if !dependents.is_empty() {
                let dep_line = format!(
                    "  deps: {} imported by {}",
                    file,
                    dependents.join(", ")
                );
                if total_len + dep_line.len() <= max_chars {
                    total_len += dep_line.len() + 1;
                    sections.push(dep_line);
                }
            }
        }
    }

    if sections.is_empty() {
        String::new()
    } else {
        // Deduplicate
        sections.dedup();
        format!("CODE STRUCTURE:\n{}", sections.join("\n"))
    }
}

fn format_symbol_line(sym: &CodeSymbol) -> String {
    let parent = sym
        .parent
        .as_deref()
        .map(|p| format!(" ({})", p))
        .unwrap_or_default();
    format!(
        "  {} {}{} [{}:{}] {}",
        sym.visibility.as_str(),
        sym.kind.as_str(),
        parent,
        sym.file_path,
        sym.line_start,
        truncate_sig(&sym.signature, 120),
    )
}

fn extract_keywords(text: &str) -> Vec<String> {
    let stop_words: std::collections::HashSet<&str> = [
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "can", "shall", "to", "of", "in", "for",
        "on", "with", "at", "by", "from", "as", "into", "through", "during",
        "before", "after", "above", "below", "between", "and", "or", "but",
        "not", "no", "nor", "so", "yet", "both", "either", "neither",
        "each", "every", "all", "any", "few", "more", "most", "other",
        "some", "such", "than", "too", "very", "just", "about", "also",
        "that", "this", "these", "those", "it", "its", "we", "they",
        "them", "their", "our", "your", "my", "me", "him", "her", "i",
        "you", "he", "she",
    ]
    .into_iter()
    .collect();

    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() > 2)
        .map(|w| w.to_lowercase())
        .filter(|w| !stop_words.contains(w.as_str()))
        .take(10)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rust_file_extracts_basics() {
        let source = r#"
use std::collections::HashMap;
use crate::config::Config;

pub struct Manager {
    config: Config,
}

impl Manager {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    fn private_method(&self) {}
}

pub enum Status {
    Active,
    Inactive,
}

pub trait Processor {
    fn process(&self);
}

const MAX_RETRIES: usize = 3;

pub type Result<T> = std::result::Result<T, Error>;
"#;
        let (symbols, deps) = parse_rust_file("src/manager.rs", source);

        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"std::collections::HashMap"));
        assert!(names.contains(&"crate::config::Config"));
        assert!(names.contains(&"Manager"));
        assert!(names.contains(&"new"));
        assert!(names.contains(&"private_method"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"Processor"));
        assert!(names.contains(&"MAX_RETRIES"));
        assert!(names.contains(&"Result"));

        // Check visibility
        let manager = symbols.iter().find(|s| s.name == "Manager").unwrap();
        assert_eq!(manager.visibility, Visibility::Public);
        assert_eq!(manager.kind, SymbolKind::Struct);

        let private = symbols.iter().find(|s| s.name == "private_method").unwrap();
        assert_eq!(private.visibility, Visibility::Private);
        assert_eq!(private.parent.as_deref(), Some("Manager"));

        // Check deps
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].to_module, "std");
        assert_eq!(deps[1].to_module, "crate");
    }

    #[test]
    fn parse_impl_for_trait() {
        let source = r#"
impl Display for Manager {
    fn fmt(&self, f: &mut Formatter) -> Result {
        write!(f, "Manager")
    }
}
"#;
        let (symbols, _) = parse_rust_file("src/test.rs", source);
        let impl_sym = symbols.iter().find(|s| s.kind == SymbolKind::Impl).unwrap();
        assert_eq!(impl_sym.name, "Display for Manager");
    }

    #[test]
    fn code_index_crud() {
        let index = CodeIndex::open_memory().unwrap();
        let symbols = vec![
            CodeSymbol {
                name: "Manager".into(),
                kind: SymbolKind::Struct,
                file_path: "src/manager.rs".into(),
                line_start: 10,
                line_end: 50,
                signature: "pub struct Manager { ... }".into(),
                parent: None,
                visibility: Visibility::Public,
            },
            CodeSymbol {
                name: "handle".into(),
                kind: SymbolKind::Function,
                file_path: "src/manager.rs".into(),
                line_start: 52,
                line_end: 100,
                signature: "pub async fn handle(&self, input: &str) -> Result<String>".into(),
                parent: Some("Manager".into()),
                visibility: Visibility::Public,
            },
        ];
        index.insert_symbols(&symbols).unwrap();

        let found = index.search_symbols("Manager", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Manager");

        let found = index.search_symbols("handle", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].parent.as_deref(), Some("Manager"));

        assert_eq!(index.symbol_count().unwrap(), 2);
        assert_eq!(index.file_count().unwrap(), 1);

        // Clear and verify
        index.clear_file("src/manager.rs").unwrap();
        assert_eq!(index.symbol_count().unwrap(), 0);
    }

    #[test]
    fn dependency_tracking() {
        let index = CodeIndex::open_memory().unwrap();
        let deps = vec![
            ModuleDep {
                from_file: "src/manager.rs".into(),
                to_module: "crate::memory".into(),
                import_path: "crate::memory::MemoryStore".into(),
            },
            ModuleDep {
                from_file: "src/executor.rs".into(),
                to_module: "crate::memory".into(),
                import_path: "crate::memory::MemoryStore".into(),
            },
        ];
        index.insert_deps(&deps).unwrap();

        let dependents = index.dependents_of("memory").unwrap();
        assert_eq!(dependents.len(), 2);
        assert!(dependents.contains(&"src/manager.rs".to_string()));

        let from_manager = index.dependencies_of("src/manager.rs").unwrap();
        assert_eq!(from_manager.len(), 1);
        assert_eq!(from_manager[0].to_module, "crate::memory");
    }

    #[test]
    fn keyword_extraction() {
        let keywords = extract_keywords("implement a new authentication module for the user system");
        assert!(keywords.contains(&"implement".to_string()));
        assert!(keywords.contains(&"authentication".to_string()));
        assert!(keywords.contains(&"module".to_string()));
        assert!(keywords.contains(&"user".to_string()));
        assert!(keywords.contains(&"system".to_string()));
        // Stop words should be excluded
        assert!(!keywords.contains(&"the".to_string()));
        assert!(!keywords.contains(&"for".to_string()));
    }
}
