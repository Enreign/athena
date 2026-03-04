use std::sync::Mutex;

use rusqlite::Connection;
use serde::Serialize;

use crate::error::{AthenaError, Result};
use crate::llm::{ChatRole, LlmProvider, Message};

// ── Event types for the activity log ────────────────────────────────

/// Categories of session events that get logged for review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityEventType {
    ChatIn,
    ChatOut,
    ToolRun,
    AutonomousTaskStart,
    AutonomousTaskFinish,
    AutonomousTaskFail,
    GhostSwitch,
    MemoryStore,
    PulseDelivered,
}

impl ActivityEventType {
    pub fn label(&self) -> &'static str {
        match self {
            Self::ChatIn => "chat_in",
            Self::ChatOut => "chat_out",
            Self::ToolRun => "tool_run",
            Self::AutonomousTaskStart => "task_start",
            Self::AutonomousTaskFinish => "task_finish",
            Self::AutonomousTaskFail => "task_fail",
            Self::GhostSwitch => "ghost_switch",
            Self::MemoryStore => "memory_store",
            Self::PulseDelivered => "pulse_delivered",
        }
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            Self::ChatIn => "💬",
            Self::ChatOut => "🤖",
            Self::ToolRun => "🔧",
            Self::AutonomousTaskStart => "🚀",
            Self::AutonomousTaskFinish => "✅",
            Self::AutonomousTaskFail => "❌",
            Self::GhostSwitch => "👻",
            Self::MemoryStore => "🧠",
            Self::PulseDelivered => "📡",
        }
    }
}

// ── Stored activity entry ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ActivityEntry {
    pub id: i64,
    pub session_key: String,
    pub event_type: String,
    pub summary: String,
    pub detail: Option<String>,
    pub ghost: Option<String>,
    pub tool_name: Option<String>,
    pub task_id: Option<String>,
    pub duration_ms: Option<i64>,
    pub created_at: String,
}

// ── Detail levels for review rendering ──────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDetail {
    /// One-paragraph executive summary.
    Summary,
    /// Timeline with key events, tools used, outcomes.
    Standard,
    /// Full detail: every event, reasoning, tool outputs.
    Detailed,
}

impl ReviewDetail {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().trim() {
            "summary" | "brief" | "tldr" => Self::Summary,
            "detailed" | "full" | "verbose" => Self::Detailed,
            _ => Self::Standard,
        }
    }
}

// ── Activity log store ──────────────────────────────────────────────

/// SQLite-backed store for session activity logs.
pub struct ActivityLogStore {
    conn: Mutex<Connection>,
}

impl ActivityLogStore {
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }

    /// Record a session activity event.
    pub fn record(
        &self,
        session_key: &str,
        event_type: ActivityEventType,
        summary: &str,
        detail: Option<&str>,
        ghost: Option<&str>,
        tool_name: Option<&str>,
        task_id: Option<&str>,
        duration_ms: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| {
            AthenaError::Tool(format!("Failed to lock activity log store: {}", e))
        })?;
        conn.execute(
            "INSERT INTO session_activity_log
             (session_key, event_type, summary, detail, ghost, tool_name, task_id, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                session_key,
                event_type.label(),
                summary,
                detail,
                ghost,
                tool_name,
                task_id,
                duration_ms,
            ],
        )?;
        Ok(())
    }

    /// Get recent activity entries for a session, newest first.
    pub fn recent(&self, session_key: &str, limit: usize) -> Result<Vec<ActivityEntry>> {
        let conn = self.conn.lock().map_err(|e| {
            AthenaError::Tool(format!("Failed to lock activity log store: {}", e))
        })?;
        let mut stmt = conn.prepare(
            "SELECT id, session_key, event_type, summary, detail, ghost, tool_name, task_id, duration_ms, created_at
             FROM session_activity_log
             WHERE session_key = ?1
             ORDER BY created_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_key, limit as i64], |row| {
            Ok(ActivityEntry {
                id: row.get(0)?,
                session_key: row.get(1)?,
                event_type: row.get(2)?,
                summary: row.get(3)?,
                detail: row.get(4)?,
                ghost: row.get(5)?,
                tool_name: row.get(6)?,
                task_id: row.get(7)?,
                duration_ms: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        entries.reverse(); // chronological order
        Ok(entries)
    }

    /// Get all activity entries across all sessions within the last N hours.
    pub fn recent_global(&self, hours: u32, limit: usize) -> Result<Vec<ActivityEntry>> {
        let conn = self.conn.lock().map_err(|e| {
            AthenaError::Tool(format!("Failed to lock activity log store: {}", e))
        })?;
        let mut stmt = conn.prepare(
            "SELECT id, session_key, event_type, summary, detail, ghost, tool_name, task_id, duration_ms, created_at
             FROM session_activity_log
             WHERE created_at > datetime('now', ?1)
             ORDER BY created_at DESC
             LIMIT ?2",
        )?;
        let age_param = format!("-{} hours", hours);
        let rows = stmt.query_map(rusqlite::params![age_param, limit as i64], |row| {
            Ok(ActivityEntry {
                id: row.get(0)?,
                session_key: row.get(1)?,
                event_type: row.get(2)?,
                summary: row.get(3)?,
                detail: row.get(4)?,
                ghost: row.get(5)?,
                tool_name: row.get(6)?,
                task_id: row.get(7)?,
                duration_ms: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        entries.reverse();
        Ok(entries)
    }

    /// Count events by type within a time window.
    pub fn event_counts(&self, session_key: &str, hours: u32) -> Result<Vec<(String, i64)>> {
        let conn = self.conn.lock().map_err(|e| {
            AthenaError::Tool(format!("Failed to lock activity log store: {}", e))
        })?;
        let mut stmt = conn.prepare(
            "SELECT event_type, COUNT(*) as cnt
             FROM session_activity_log
             WHERE session_key = ?1 AND created_at > datetime('now', ?2)
             GROUP BY event_type
             ORDER BY cnt DESC",
        )?;
        let age_param = format!("-{} hours", hours);
        let rows = stmt.query_map(rusqlite::params![session_key, age_param], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut counts = Vec::new();
        for row in rows {
            counts.push(row?);
        }
        Ok(counts)
    }

    /// Get distinct tools used in recent session activity.
    pub fn tools_used(&self, session_key: &str, hours: u32) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| {
            AthenaError::Tool(format!("Failed to lock activity log store: {}", e))
        })?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT tool_name
             FROM session_activity_log
             WHERE session_key = ?1
               AND tool_name IS NOT NULL
               AND created_at > datetime('now', ?2)
             ORDER BY tool_name",
        )?;
        let age_param = format!("-{} hours", hours);
        let rows = stmt.query_map(rusqlite::params![session_key, age_param], |row| {
            row.get::<_, String>(0)
        })?;
        let mut tools = Vec::new();
        for row in rows {
            tools.push(row?);
        }
        Ok(tools)
    }

    /// Delete old activity entries (older than N days).
    pub fn cleanup(&self, max_age_days: i64) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| {
            AthenaError::Tool(format!("Failed to lock activity log store: {}", e))
        })?;
        let deleted = conn.execute(
            "DELETE FROM session_activity_log WHERE created_at < datetime('now', ?1)",
            rusqlite::params![format!("-{} days", max_age_days)],
        )?;
        Ok(deleted)
    }
}

// ── Review rendering ────────────────────────────────────────────────

/// Render a structured review of session activity (no LLM needed).
pub fn render_review(entries: &[ActivityEntry], detail: ReviewDetail) -> String {
    if entries.is_empty() {
        return "No activity recorded for this session yet.".to_string();
    }

    match detail {
        ReviewDetail::Summary => render_summary(entries),
        ReviewDetail::Standard => render_standard(entries),
        ReviewDetail::Detailed => render_detailed(entries),
    }
}

fn render_summary(entries: &[ActivityEntry]) -> String {
    let chat_in = entries.iter().filter(|e| e.event_type == "chat_in").count();
    let chat_out = entries.iter().filter(|e| e.event_type == "chat_out").count();
    let tools = entries.iter().filter(|e| e.event_type == "tool_run").count();
    let tasks_started = entries
        .iter()
        .filter(|e| e.event_type == "task_start")
        .count();
    let tasks_ok = entries
        .iter()
        .filter(|e| e.event_type == "task_finish")
        .count();
    let tasks_fail = entries
        .iter()
        .filter(|e| e.event_type == "task_fail")
        .count();

    let time_range = if entries.len() >= 2 {
        format!(
            "{} → {}",
            &entries[0].created_at,
            &entries[entries.len() - 1].created_at
        )
    } else {
        entries[0].created_at.clone()
    };

    let unique_tools: Vec<String> = {
        let mut t: Vec<String> = entries
            .iter()
            .filter_map(|e| e.tool_name.clone())
            .collect();
        t.sort();
        t.dedup();
        t
    };

    let unique_ghosts: Vec<String> = {
        let mut g: Vec<String> = entries
            .iter()
            .filter_map(|e| e.ghost.clone())
            .collect();
        g.sort();
        g.dedup();
        g
    };

    let mut out = String::new();
    out.push_str(&format!("<b>📋 Session Summary</b>\n"));
    out.push_str(&format!("⏱ {}\n\n", time_range));
    out.push_str(&format!(
        "💬 {} messages in, {} responses out\n",
        chat_in, chat_out
    ));
    out.push_str(&format!("🔧 {} tool executions\n", tools));
    if tasks_started > 0 {
        out.push_str(&format!(
            "🚀 {} tasks dispatched (✅{} ❌{})\n",
            tasks_started, tasks_ok, tasks_fail
        ));
    }
    if !unique_tools.is_empty() {
        out.push_str(&format!("🛠 Tools: {}\n", unique_tools.join(", ")));
    }
    if !unique_ghosts.is_empty() {
        out.push_str(&format!("👻 Ghosts: {}\n", unique_ghosts.join(", ")));
    }
    out
}

fn render_standard(entries: &[ActivityEntry]) -> String {
    let mut out = render_summary(entries);
    out.push_str("\n<b>📜 Timeline</b>\n");
    for entry in entries {
        let emoji = type_emoji(&entry.event_type);
        let time = entry
            .created_at
            .split_whitespace()
            .nth(1)
            .unwrap_or(&entry.created_at);
        let mut line = format!("<code>{}</code> {} {}", time, emoji, entry.summary);
        if let Some(ref ghost) = entry.ghost {
            line.push_str(&format!(" [{}]", ghost));
        }
        if let Some(ms) = entry.duration_ms {
            line.push_str(&format!(" ({}ms)", ms));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn render_detailed(entries: &[ActivityEntry]) -> String {
    let mut out = render_summary(entries);
    out.push_str("\n<b>📜 Detailed Log</b>\n\n");
    for entry in entries {
        let emoji = type_emoji(&entry.event_type);
        out.push_str(&format!(
            "<b>{} [{}]</b> {}\n",
            emoji, entry.created_at, entry.summary
        ));
        if let Some(ref ghost) = entry.ghost {
            out.push_str(&format!("  👻 Ghost: {}\n", ghost));
        }
        if let Some(ref tool) = entry.tool_name {
            out.push_str(&format!("  🛠 Tool: {}\n", tool));
        }
        if let Some(ref task_id) = entry.task_id {
            out.push_str(&format!("  🆔 Task: {}\n", task_id));
        }
        if let Some(ms) = entry.duration_ms {
            out.push_str(&format!("  ⏱ Duration: {}ms\n", ms));
        }
        if let Some(ref detail) = entry.detail {
            let truncated = if detail.len() > 500 {
                format!("{}...", &detail[..500])
            } else {
                detail.clone()
            };
            out.push_str(&format!("  📝 {}\n", truncated));
        }
        out.push('\n');
    }
    out
}

fn type_emoji(event_type: &str) -> &'static str {
    match event_type {
        "chat_in" => "💬",
        "chat_out" => "🤖",
        "tool_run" => "🔧",
        "task_start" => "🚀",
        "task_finish" => "✅",
        "task_fail" => "❌",
        "ghost_switch" => "👻",
        "memory_store" => "🧠",
        "pulse_delivered" => "📡",
        _ => "•",
    }
}

// ── LLM-powered conceptual explanation ──────────────────────────────

/// Generate a conceptual explanation of recent activity using LLM.
pub async fn generate_explanation(
    entries: &[ActivityEntry],
    llm: &dyn LlmProvider,
    detail: ReviewDetail,
) -> Result<String> {
    if entries.is_empty() {
        return Ok("No activity to explain.".to_string());
    }

    let activity_dump = entries
        .iter()
        .map(|e| {
            let mut line = format!("[{}] {} — {}", e.created_at, e.event_type, e.summary);
            if let Some(ref d) = e.detail {
                let trunc = if d.len() > 300 {
                    format!("{}...", &d[..300])
                } else {
                    d.clone()
                };
                line.push_str(&format!(" | {}", trunc));
            }
            if let Some(ref g) = e.ghost {
                line.push_str(&format!(" [ghost:{}]", g));
            }
            if let Some(ref t) = e.tool_name {
                line.push_str(&format!(" [tool:{}]", t));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = match detail {
        ReviewDetail::Summary => format!(
            "You are explaining what an AI agent system (Athena) did during a session.\n\
             Give a concise 2-3 sentence conceptual summary. Focus on WHAT was accomplished \
             and WHY, not implementation details. Write for a product manager.\n\n\
             Activity log:\n{}\n\nExplain:",
            activity_dump
        ),
        ReviewDetail::Standard => format!(
            "You are explaining what an AI agent system (Athena) did during a session.\n\
             Give a structured explanation with:\n\
             1. **What happened** — conceptual overview (2-3 sentences)\n\
             2. **Key decisions** — what reasoning drove the actions\n\
             3. **Tools & ghosts used** — why each was chosen\n\
             4. **Outcome** — what was achieved\n\n\
             Write at a logic/concept level, not code level. \
             Make it understandable for an engineer reviewing the session.\n\n\
             Activity log:\n{}\n\nExplain:",
            activity_dump
        ),
        ReviewDetail::Detailed => format!(
            "You are explaining what an AI agent system (Athena) did during a session.\n\
             Give a comprehensive explanation with:\n\
             1. **Executive summary** — 2-3 sentence overview\n\
             2. **Phase breakdown** — group events into logical phases, explain each\n\
             3. **Decision reasoning** — for each major decision, explain why\n\
             4. **Tool chain analysis** — what tools were used in what sequence and why\n\
             5. **Ghost strategy** — which agents were invoked and the reasoning\n\
             6. **What could be improved** — any inefficiencies or concerns\n\
             7. **How this fits the bigger picture** — connect to the overall system goals\n\n\
             Be thorough but conceptual. Write for a senior engineer who wants \
             full understanding without reading raw logs.\n\n\
             Activity log:\n{}\n\nExplain:",
            activity_dump
        ),
    };

    let messages = vec![Message {
        role: ChatRole::User,
        content: prompt,
    }];

    let response = llm.chat(&messages).await?;
    Ok(response)
}

// ── Period summary generation ───────────────────────────────────────

/// Generate a summary of all activity over a time period.
pub async fn generate_period_summary(
    entries: &[ActivityEntry],
    hours: u32,
    llm: &dyn LlmProvider,
) -> Result<String> {
    if entries.is_empty() {
        return Ok(format!("No activity in the last {} hours.", hours));
    }

    let stats = compute_period_stats(entries);
    let activity_dump = entries
        .iter()
        .map(|e| format!("[{}] {} — {}", e.created_at, e.event_type, e.summary))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "You are summarizing what an AI agent system (Athena) accomplished over the last {} hours.\n\n\
         Statistics:\n{}\n\n\
         Full activity log:\n{}\n\n\
         Write a comprehensive period summary covering:\n\
         1. **Overview** — what was accomplished in this period\n\
         2. **Key milestones** — important completions or decisions\n\
         3. **Active areas** — which parts of the system were most active\n\
         4. **Health** — any failures, retries, or concerns\n\
         5. **Recommendations** — what should be reviewed or followed up\n\n\
         Write for a product manager doing a daily review.",
        hours, stats, activity_dump
    );

    let messages = vec![Message {
        role: ChatRole::User,
        content: prompt,
    }];

    let response = llm.chat(&messages).await?;
    Ok(response)
}

fn compute_period_stats(entries: &[ActivityEntry]) -> String {
    let total = entries.len();
    let chat_in = entries.iter().filter(|e| e.event_type == "chat_in").count();
    let chat_out = entries.iter().filter(|e| e.event_type == "chat_out").count();
    let tools = entries.iter().filter(|e| e.event_type == "tool_run").count();
    let tasks_started = entries.iter().filter(|e| e.event_type == "task_start").count();
    let tasks_ok = entries.iter().filter(|e| e.event_type == "task_finish").count();
    let tasks_fail = entries.iter().filter(|e| e.event_type == "task_fail").count();

    let unique_ghosts: Vec<String> = {
        let mut g: Vec<String> = entries.iter().filter_map(|e| e.ghost.clone()).collect();
        g.sort();
        g.dedup();
        g
    };

    let unique_tools: Vec<String> = {
        let mut t: Vec<String> = entries.iter().filter_map(|e| e.tool_name.clone()).collect();
        t.sort();
        t.dedup();
        t
    };

    format!(
        "Total events: {}\n\
         Chat: {} in, {} out\n\
         Tool runs: {}\n\
         Tasks: {} started, {} succeeded, {} failed\n\
         Ghosts active: {}\n\
         Tools used: {}",
        total,
        chat_in,
        chat_out,
        tools,
        tasks_started,
        tasks_ok,
        tasks_fail,
        unique_ghosts.join(", "),
        unique_tools.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> ActivityLogStore {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE session_activity_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_key TEXT NOT NULL,
                event_type TEXT NOT NULL,
                summary TEXT NOT NULL,
                detail TEXT,
                ghost TEXT,
                tool_name TEXT,
                task_id TEXT,
                duration_ms INTEGER,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX idx_session_activity_session_time
                ON session_activity_log(session_key, created_at DESC);",
        )
        .unwrap();
        ActivityLogStore::new(conn)
    }

    #[test]
    fn test_record_and_recent() {
        let store = test_store();
        store
            .record(
                "tg:123:456",
                ActivityEventType::ChatIn,
                "User asked about deployment",
                Some("How do I deploy to prod?"),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        store
            .record(
                "tg:123:456",
                ActivityEventType::ChatOut,
                "Explained deployment process",
                Some("To deploy to production, you need to..."),
                Some("mentor"),
                None,
                None,
                Some(1200),
            )
            .unwrap();
        store
            .record(
                "tg:123:456",
                ActivityEventType::ToolRun,
                "Executed shell command",
                Some("git status"),
                None,
                Some("shell"),
                None,
                Some(45),
            )
            .unwrap();

        let entries = store.recent("tg:123:456", 10).unwrap();
        assert_eq!(entries.len(), 3);
        // Check that all event types are present (order depends on timestamp resolution)
        let types: Vec<&str> = entries.iter().map(|e| e.event_type.as_str()).collect();
        assert!(types.contains(&"chat_in"));
        assert!(types.contains(&"chat_out"));
        assert!(types.contains(&"tool_run"));
        // Check tool_name on the tool_run entry
        let tool_entry = entries.iter().find(|e| e.event_type == "tool_run").unwrap();
        assert_eq!(tool_entry.tool_name.as_deref(), Some("shell"));
    }

    #[test]
    fn test_event_counts() {
        let store = test_store();
        for _ in 0..3 {
            store
                .record(
                    "s1",
                    ActivityEventType::ChatIn,
                    "msg",
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
        }
        store
            .record(
                "s1",
                ActivityEventType::ToolRun,
                "tool",
                None,
                None,
                Some("shell"),
                None,
                None,
            )
            .unwrap();

        let counts = store.event_counts("s1", 24).unwrap();
        assert!(counts.len() >= 2);
    }

    #[test]
    fn test_tools_used() {
        let store = test_store();
        store
            .record(
                "s1",
                ActivityEventType::ToolRun,
                "a",
                None,
                None,
                Some("shell"),
                None,
                None,
            )
            .unwrap();
        store
            .record(
                "s1",
                ActivityEventType::ToolRun,
                "b",
                None,
                None,
                Some("git"),
                None,
                None,
            )
            .unwrap();
        store
            .record(
                "s1",
                ActivityEventType::ToolRun,
                "c",
                None,
                None,
                Some("shell"),
                None,
                None,
            )
            .unwrap();

        let tools = store.tools_used("s1", 24).unwrap();
        assert_eq!(tools, vec!["git", "shell"]);
    }

    #[test]
    fn test_render_summary() {
        let entries = vec![
            ActivityEntry {
                id: 1,
                session_key: "s1".into(),
                event_type: "chat_in".into(),
                summary: "Asked about X".into(),
                detail: None,
                ghost: None,
                tool_name: None,
                task_id: None,
                duration_ms: None,
                created_at: "2025-01-01 10:00:00".into(),
            },
            ActivityEntry {
                id: 2,
                session_key: "s1".into(),
                event_type: "tool_run".into(),
                summary: "Ran shell cmd".into(),
                detail: None,
                ghost: None,
                tool_name: Some("shell".into()),
                task_id: None,
                duration_ms: Some(50),
                created_at: "2025-01-01 10:01:00".into(),
            },
            ActivityEntry {
                id: 3,
                session_key: "s1".into(),
                event_type: "chat_out".into(),
                summary: "Replied with answer".into(),
                detail: None,
                ghost: Some("mentor".into()),
                tool_name: None,
                task_id: None,
                duration_ms: Some(800),
                created_at: "2025-01-01 10:02:00".into(),
            },
        ];

        let summary = render_review(&entries, ReviewDetail::Summary);
        assert!(summary.contains("Session Summary"));
        assert!(summary.contains("1 messages in"));
        assert!(summary.contains("1 tool executions"));

        let standard = render_review(&entries, ReviewDetail::Standard);
        assert!(standard.contains("Timeline"));
        assert!(standard.contains("shell"));
    }

    #[test]
    fn test_render_empty() {
        let result = render_review(&[], ReviewDetail::Summary);
        assert!(result.contains("No activity"));
    }
}
