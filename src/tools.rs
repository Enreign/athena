use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::config::GhostConfig;
use crate::docker::DockerSession;
use crate::error::{AthenaError, Result};

const MAX_OUTPUT_LEN: usize = 2000;
const SEARCH_OUTPUT_LEN: usize = 8000;
const GLOB_OUTPUT_LEN: usize = 4000;

#[derive(Debug)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn needs_confirmation(&self) -> bool;
    async fn execute(&self, session: &DockerSession, params: &Value) -> Result<ToolResult>;
}

/// Sensitive filenames that should never be read or written inside containers
const SENSITIVE_FILENAMES: &[&str] = &[
    "config.toml",
    ".env",
    ".env.local",
    "credentials.json",
    "secrets.toml",
];

/// Sensitive file extensions
const SENSITIVE_EXTENSIONS: &[&str] = &[".pem", ".key"];

/// Validate a path for safety: no traversal, must be under /workspace, no sensitive files
fn validate_path(path: &str) -> std::result::Result<(), &'static str> {
    // Reject path traversal
    if path.contains("..") {
        return Err("Path traversal (..) not allowed");
    }

    // Reject absolute paths outside /workspace
    if path.starts_with('/') && !path.starts_with("/workspace") {
        return Err("Absolute paths must be under /workspace");
    }

    // Check filename against sensitive names
    let filename = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    for &sensitive in SENSITIVE_FILENAMES {
        if filename == sensitive {
            return Err("Access to sensitive file denied");
        }
    }

    for &ext in SENSITIVE_EXTENSIONS {
        if filename.ends_with(ext) {
            return Err("Access to sensitive file type denied");
        }
    }

    Ok(())
}

/// Validate a URL for safety: must be http(s), no private/internal IPs (SSRF protection)
fn validate_url(url: &str) -> std::result::Result<(), &'static str> {
    // Must start with http:// or https://
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("URL must use http:// or https:// scheme");
    }

    // Extract host from URL
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");

    let host_lower = host.to_lowercase();

    // Block localhost
    if host_lower == "localhost" || host_lower == "127.0.0.1" || host_lower == "0.0.0.0"
        || host_lower == "::1" || host_lower == "[::1]"
    {
        return Err("Access to localhost denied");
    }

    // Block private IP ranges (10.x.x.x, 172.16-31.x.x, 192.168.x.x, 169.254.x.x)
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        let octets = ip.octets();
        if octets[0] == 10
            || (octets[0] == 172 && (16..=31).contains(&octets[1]))
            || (octets[0] == 192 && octets[1] == 168)
            || (octets[0] == 169 && octets[1] == 254)
        {
            return Err("Access to private IP ranges denied");
        }
    }

    Ok(())
}

/// Truncate output to prevent context bloat
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...\n[truncated, {} total chars]", &s[..max], s.len())
    }
}

// ── Shell tool ──────────────────────────────────────────────────────

struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str { "shell" }
    fn description(&self) -> &str { "Run a shell command: {\"tool\": \"shell\", \"params\": {\"command\": \"...\"}}" }
    fn needs_confirmation(&self) -> bool { false } // Handled by sensitive pattern check in strategy

    async fn execute(&self, session: &DockerSession, params: &Value) -> Result<ToolResult> {
        let cmd = params.get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AthenaError::Tool("shell: missing 'command' param".into()))?;

        let output = session.exec(cmd).await?;
        Ok(ToolResult {
            success: true,
            output: truncate(&output, MAX_OUTPUT_LEN),
        })
    }
}

// ── FileRead tool ───────────────────────────────────────────────────

struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str { "file_read" }
    fn description(&self) -> &str { "Read a file: {\"tool\": \"file_read\", \"params\": {\"path\": \"...\"}}" }
    fn needs_confirmation(&self) -> bool { false }

    async fn execute(&self, session: &DockerSession, params: &Value) -> Result<ToolResult> {
        let path = params.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AthenaError::Tool("file_read: missing 'path' param".into()))?;

        if let Err(reason) = validate_path(path) {
            return Ok(ToolResult {
                success: false,
                output: reason.into(),
            });
        }

        let cmd = format!("cat '{}'", path.replace('\'', "'\\''"));
        let output = session.exec(&cmd).await?;
        Ok(ToolResult {
            success: true,
            output: truncate(&output, MAX_OUTPUT_LEN),
        })
    }
}

// ── FileWrite tool ──────────────────────────────────────────────────

struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str { "file_write" }
    fn description(&self) -> &str { "Write a file: {\"tool\": \"file_write\", \"params\": {\"path\": \"...\", \"content\": \"...\"}}" }
    fn needs_confirmation(&self) -> bool { true }

    async fn execute(&self, session: &DockerSession, params: &Value) -> Result<ToolResult> {
        let path = params.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AthenaError::Tool("file_write: missing 'path' param".into()))?;
        let content = params.get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AthenaError::Tool("file_write: missing 'content' param".into()))?;

        if let Err(reason) = validate_path(path) {
            return Ok(ToolResult {
                success: false,
                output: reason.into(),
            });
        }

        let write_cmd = format!("cat > '{}'", path.replace('\'', "'\\''"));
        session.exec_with_stdin(&write_cmd, content).await?;

        Ok(ToolResult {
            success: true,
            output: format!("Wrote {} bytes to {}", content.len(), path),
        })
    }
}

// ── FileEdit tool ───────────────────────────────────────────────────

struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str { "file_edit" }
    fn description(&self) -> &str {
        "Edit a file by replacing a string: {\"tool\": \"file_edit\", \"params\": {\"path\": \"...\", \"old_string\": \"...\", \"new_string\": \"...\"}}"
    }
    fn needs_confirmation(&self) -> bool { true }

    async fn execute(&self, session: &DockerSession, params: &Value) -> Result<ToolResult> {
        let path = params.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AthenaError::Tool("file_edit: missing 'path' param".into()))?;
        let old_string = params.get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AthenaError::Tool("file_edit: missing 'old_string' param".into()))?;
        let new_string = params.get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AthenaError::Tool("file_edit: missing 'new_string' param".into()))?;

        if let Err(reason) = validate_path(path) {
            return Ok(ToolResult {
                success: false,
                output: reason.into(),
            });
        }

        // Read the file
        let cat_cmd = format!("cat '{}'", path.replace('\'', "'\\''"));
        let content = session.exec(&cat_cmd).await?;

        // Check that old_string exists
        let count = content.matches(old_string).count();
        if count == 0 {
            return Ok(ToolResult {
                success: false,
                output: format!("file_edit: '{}' not found in {}", old_string, path),
            });
        }
        if count > 1 {
            return Ok(ToolResult {
                success: false,
                output: format!(
                    "file_edit: '{}' found {} times in {} (must be unique, provide more context)",
                    old_string, count, path
                ),
            });
        }

        // Replace (exactly one match)
        let new_content = content.replacen(old_string, new_string, 1);

        // Write back
        let write_cmd = format!("cat > '{}'", path.replace('\'', "'\\''"));
        session.exec_with_stdin(&write_cmd, &new_content).await?;

        Ok(ToolResult {
            success: true,
            output: truncate(
                &format!("Edited {}:\n- {}\n+ {}", path, old_string, new_string),
                MAX_OUTPUT_LEN,
            ),
        })
    }
}

// ── Grep tool ───────────────────────────────────────────────────────

struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str { "grep" }
    fn description(&self) -> &str {
        "Search file contents: {\"tool\": \"grep\", \"params\": {\"pattern\": \"...\", \"path\": \".\", \"include\": \"*.rs\"}}"
    }
    fn needs_confirmation(&self) -> bool { false }

    async fn execute(&self, session: &DockerSession, params: &Value) -> Result<ToolResult> {
        let pattern = params.get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AthenaError::Tool("grep: missing 'pattern' param".into()))?;
        let path = params.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");
        let include = params.get("include")
            .and_then(|v| v.as_str());

        // Validate search path
        if path != "." {
            if let Err(reason) = validate_path(path) {
                return Ok(ToolResult {
                    success: false,
                    output: reason.into(),
                });
            }
        }

        // Build grep command: -r recursive, -n line numbers
        // Shell-escape the pattern by using -- to end options
        let escaped_path = path.replace('\'', "'\\''");
        let escaped_pattern = pattern.replace('\'', "'\\''");

        let mut cmd = format!("grep -rn -- '{}' '{}'", escaped_pattern, escaped_path);

        if let Some(inc) = include {
            let escaped_inc = inc.replace('\'', "'\\''");
            cmd = format!("grep -rn --include='{}' -- '{}' '{}'", escaped_inc, escaped_pattern, escaped_path);
        }

        // Limit output lines
        cmd = format!("{} | head -50", cmd);

        let output = session.exec(&cmd).await?;
        if output.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: format!("No matches for '{}' in {}", pattern, path),
            });
        }

        Ok(ToolResult {
            success: true,
            output: truncate(&output, SEARCH_OUTPUT_LEN),
        })
    }
}

// ── Glob tool ───────────────────────────────────────────────────────

struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str { "glob" }
    fn description(&self) -> &str {
        "Find files by pattern: {\"tool\": \"glob\", \"params\": {\"pattern\": \"*.rs\", \"path\": \".\"}}"
    }
    fn needs_confirmation(&self) -> bool { false }

    async fn execute(&self, session: &DockerSession, params: &Value) -> Result<ToolResult> {
        let pattern = params.get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AthenaError::Tool("glob: missing 'pattern' param".into()))?;
        let path = params.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        // Validate search path
        if path != "." {
            if let Err(reason) = validate_path(path) {
                return Ok(ToolResult {
                    success: false,
                    output: reason.into(),
                });
            }
        }

        // Extract just the filename pattern (e.g., "**/*.rs" -> "*.rs")
        let name_pattern = pattern.rsplit('/').next().unwrap_or(pattern);

        let escaped_path = path.replace('\'', "'\\''");
        let escaped_name = name_pattern.replace('\'', "'\\''");

        let cmd = format!(
            "find '{}' -name '{}' -type f 2>/dev/null | head -100 | sort",
            escaped_path, escaped_name
        );

        let output = session.exec(&cmd).await?;
        if output.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: format!("No files matching '{}' in {}", pattern, path),
            });
        }

        Ok(ToolResult {
            success: true,
            output: truncate(&output, GLOB_OUTPUT_LEN),
        })
    }
}

// ── WebFetch tool ───────────────────────────────────────────────────

struct WebFetchTool {
    client: reqwest::Client,
}

impl WebFetchTool {
    fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("Athena/0.1")
            .build()
            .expect("failed to build reqwest client");
        Self { client }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str { "web_fetch" }
    fn description(&self) -> &str {
        "Fetch a URL: {\"tool\": \"web_fetch\", \"params\": {\"url\": \"https://...\"}}"
    }
    fn needs_confirmation(&self) -> bool { false }

    async fn execute(&self, _session: &DockerSession, params: &Value) -> Result<ToolResult> {
        let url = params.get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AthenaError::Tool("web_fetch: missing 'url' param".into()))?;

        if let Err(reason) = validate_url(url) {
            return Ok(ToolResult {
                success: false,
                output: reason.into(),
            });
        }

        let response = self.client
            .get(url)
            .send()
            .await
            .map_err(|e| AthenaError::Tool(format!("web_fetch: request failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            return Ok(ToolResult {
                success: false,
                output: format!("web_fetch: HTTP {}", status),
            });
        }

        // Limit body size to 1MB
        let bytes = response
            .bytes()
            .await
            .map_err(|e| AthenaError::Tool(format!("web_fetch: read failed: {}", e)))?;

        if bytes.len() > 1_048_576 {
            return Ok(ToolResult {
                success: false,
                output: "web_fetch: response too large (>1MB)".into(),
            });
        }

        let body = String::from_utf8_lossy(&bytes).to_string();

        // Strip HTML tags and clean up
        let re_tags = regex::Regex::new(r"<[^>]+>").unwrap();
        let text = re_tags.replace_all(&body, "");

        // Decode common HTML entities
        let text = text
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&nbsp;", " ");

        // Collapse whitespace
        let re_ws = regex::Regex::new(r"\s+").unwrap();
        let text = re_ws.replace_all(&text, " ");
        let text = text.trim();

        Ok(ToolResult {
            success: true,
            output: truncate(text, SEARCH_OUTPUT_LEN),
        })
    }
}

// ── Registry ────────────────────────────────────────────────────────

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Build a registry scoped to a ghost's allowed tools
    pub fn for_ghost(ghost: &GhostConfig) -> Self {
        let all_tools: Vec<Box<dyn Tool>> = vec![
            Box::new(ShellTool),
            Box::new(FileReadTool),
            Box::new(FileWriteTool),
            Box::new(FileEditTool),
            Box::new(GrepTool),
            Box::new(GlobTool),
            Box::new(WebFetchTool::new()),
        ];

        let tools: HashMap<String, Box<dyn Tool>> = all_tools
            .into_iter()
            .filter(|t| ghost.tools.contains(&t.name().to_string()))
            .map(|t| (t.name().to_string(), t))
            .collect();

        Self { tools }
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|b| b.as_ref())
    }

    /// Format tool descriptions for the LLM system prompt
    pub fn descriptions(&self) -> String {
        self.tools.values()
            .map(|t| format!("- {}", t.description()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }
}
