//! Multi-Pass Scout Strategy
//!
//! A three-pass context refinement strategy that compiles precise context
//! before delegating to a coding tool:
//!
//! 1. **Scout pass** — lightweight LLM call with minimal context (file tree +
//!    code index summary) produces a `ContextRequest`: which files, symbols,
//!    and patterns the task needs.
//! 2. **Compile pass** — pure Rust, no LLM. Fulfills the ContextRequest by
//!    reading files, querying the code index, and running targeted grep.
//!    Produces a compiled context pack.
//! 3. **Execute pass** — full model gets the compiled context pack + task goal
//!    and executes via the normal code strategy or tool call.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::confirm::Confirmer;
use crate::core::CoreEvent;
use crate::docker::DockerSession;
use crate::error::{AthenaError, Result};
use crate::executor::Executor;
use crate::langfuse::ActiveTrace;
use crate::llm::{self, ChatMessage, ChatResponse, LlmProvider, StreamEvent};
use crate::tools::ToolRegistry;

use super::{materialize_tool_result, LoopStrategy, StatusSender, TaskContract};

/// Maximum number of files the scout can request.
const MAX_SCOUT_FILES: usize = 15;
/// Maximum number of grep patterns the scout can request.
const MAX_SCOUT_PATTERNS: usize = 8;
/// Maximum chars for compiled context to avoid blowing the execution budget.
const MAX_COMPILED_CONTEXT_CHARS: usize = 40_000;

// ---------------------------------------------------------------------------
// Context Request — output of the scout pass
// ---------------------------------------------------------------------------

/// Structured request from the scout LLM describing what context it needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextRequest {
    /// Specific files to read (full or partial paths).
    #[serde(default)]
    pub files: Vec<String>,
    /// Symbol names to look up in the code index.
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Grep patterns to search for across the codebase.
    #[serde(default)]
    pub grep_patterns: Vec<String>,
    /// Specific line ranges within files: "path:start-end"
    #[serde(default)]
    pub line_ranges: Vec<String>,
    /// Brief explanation of what the scout found relevant.
    #[serde(default)]
    pub reasoning: String,
}

// ---------------------------------------------------------------------------
// Compiled Context — output of the compile pass
// ---------------------------------------------------------------------------

/// Compiled context pack assembled by fulfilling a ContextRequest.
#[derive(Debug, Clone)]
pub struct CompiledContext {
    /// File contents (path → content snippet)
    pub file_snippets: Vec<(String, String)>,
    /// Symbol information from the code index
    pub symbol_context: String,
    /// Grep results
    pub grep_results: Vec<(String, String)>,
    /// Total chars in the compiled pack
    pub total_chars: usize,
}

impl CompiledContext {
    pub fn to_context_string(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        if !self.file_snippets.is_empty() {
            let mut file_section = String::from("RELEVANT FILES:\n");
            for (path, content) in &self.file_snippets {
                file_section.push_str(&format!("--- {} ---\n{}\n", path, content));
            }
            parts.push(file_section);
        }

        if !self.symbol_context.is_empty() {
            parts.push(self.symbol_context.clone());
        }

        if !self.grep_results.is_empty() {
            let mut grep_section = String::from("SEARCH RESULTS:\n");
            for (pattern, results) in &self.grep_results {
                grep_section.push_str(&format!("grep '{}' →\n{}\n", pattern, results));
            }
            parts.push(grep_section);
        }

        parts.join("\n\n")
    }
}

// ---------------------------------------------------------------------------
// Scout Strategy
// ---------------------------------------------------------------------------

pub struct ScoutStrategy;

#[async_trait]
impl LoopStrategy for ScoutStrategy {
    async fn run(
        &self,
        contract: &TaskContract,
        tools: &ToolRegistry,
        docker: &DockerSession,
        llm: &dyn LlmProvider,
        max_steps: usize,
        executor: &Executor,
        confirmer: &dyn Confirmer,
        status_tx: Option<&StatusSender>,
        trace: Option<&ActiveTrace>,
    ) -> Result<String> {
        // --- Pass 1: Scout ---
        if let Some(tx) = status_tx {
            let _ = tx
                .send(CoreEvent::Status("Scout: analyzing task...".to_string()))
                .await;
        }

        let scout_span = trace.map(|t| t.span("scout_pass", Some(&contract.goal)));

        let context_request = run_scout_pass(contract, tools, docker, llm, executor).await?;

        if let Some(s) = scout_span {
            s.end(Some(&format!(
                "files={} symbols={} patterns={}",
                context_request.files.len(),
                context_request.symbols.len(),
                context_request.grep_patterns.len()
            )));
        }

        tracing::info!(
            files = context_request.files.len(),
            symbols = context_request.symbols.len(),
            patterns = context_request.grep_patterns.len(),
            reasoning = %context_request.reasoning,
            "Scout pass complete"
        );

        // --- Pass 2: Compile (no LLM — pure Rust) ---
        if let Some(tx) = status_tx {
            let _ = tx
                .send(CoreEvent::Status("Compiling context...".to_string()))
                .await;
        }

        let compile_span = trace.map(|t| t.span("compile_pass", None));

        let compiled = compile_context(
            &context_request,
            tools,
            docker,
            executor,
            confirmer,
            status_tx,
            trace,
        )
        .await?;

        if let Some(s) = compile_span {
            s.end(Some(&format!("{} chars compiled", compiled.total_chars)));
        }

        tracing::info!(
            total_chars = compiled.total_chars,
            file_snippets = compiled.file_snippets.len(),
            grep_results = compiled.grep_results.len(),
            "Compile pass complete"
        );

        // --- Pass 3: Execute with compiled context ---
        if let Some(tx) = status_tx {
            let _ = tx
                .send(CoreEvent::Status("Executing with compiled context...".to_string()))
                .await;
        }

        let execute_span = trace.map(|t| t.span("execute_pass", Some(&contract.goal)));

        let compiled_context = compiled.to_context_string();
        let enriched_contract = TaskContract {
            context: format!(
                "{}\n\n{}\n\nScout reasoning: {}",
                contract.context, compiled_context, context_request.reasoning
            ),
            goal: contract.goal.clone(),
            constraints: contract.constraints.clone(),
            soul: contract.soul.clone(),
            skill: contract.skill.clone(),
            tools_doc: contract.tools_doc.clone(),
            cli_tool_preference: contract.cli_tool_preference.clone(),
            cli_tool_routing_order: contract.cli_tool_routing_order.clone(),
            test_generation: contract.test_generation,
            memory: contract.memory.clone(),
        };

        // Delegate to the code strategy for actual execution
        let code_strategy = super::code::CodeStrategy;
        let result = code_strategy
            .run(
                &enriched_contract,
                tools,
                docker,
                llm,
                max_steps,
                executor,
                confirmer,
                status_tx,
                trace,
            )
            .await?;

        if let Some(s) = execute_span {
            let preview = if result.len() > 200 {
                &result[..result.floor_char_boundary(200)]
            } else {
                &result
            };
            s.end(Some(preview));
        }

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Pass 1: Scout — lightweight LLM call to identify needed context
// ---------------------------------------------------------------------------

async fn run_scout_pass(
    contract: &TaskContract,
    tools: &ToolRegistry,
    docker: &DockerSession,
    llm: &dyn LlmProvider,
    executor: &Executor,
) -> Result<ContextRequest> {
    // Build a lightweight context: file tree + existing contract context
    let file_tree = get_file_tree(tools, docker, executor).await;

    let scout_system = format!(
        r#"You are a scout agent. Your job is to analyze a coding task and determine exactly what
source code context is needed to accomplish it. You do NOT execute the task — you only
identify what files, symbols, and patterns need to be examined.

AVAILABLE PROJECT FILES:
{}

EXISTING CONTEXT:
{}

Respond with ONLY a JSON object (no other text):
{{
  "files": ["path/to/file1.rs", "path/to/file2.rs"],
  "symbols": ["StructName", "function_name", "TraitName"],
  "grep_patterns": ["pattern_to_search"],
  "line_ranges": ["src/file.rs:100-200"],
  "reasoning": "Brief explanation of what's relevant and why"
}}

RULES:
- Request at most {} files (prefer fewer, more targeted)
- Request at most {} grep patterns
- Focus on files that will be MODIFIED or that define types/functions used by the target code
- Include test files only if the task involves testing
- Be specific: prefer "src/manager.rs" over "src/"
- For grep patterns, use specific identifiers, not generic terms"#,
        file_tree,
        contract.context,
        MAX_SCOUT_FILES,
        MAX_SCOUT_PATTERNS,
    );

    let messages = vec![
        ChatMessage::System(scout_system),
        ChatMessage::User(format!("TASK: {}", contract.goal)),
    ];

    let (response, _usage) = llm.chat_with_tools(&messages, &[]).await?;
    let response_text = match response {
        ChatResponse::Text(text) => text,
        ChatResponse::ToolCalls { text, .. } => text.unwrap_or_default(),
    };

    parse_context_request(&response_text)
}

fn parse_context_request(response: &str) -> Result<ContextRequest> {
    // Try to extract JSON from the response
    if let Some(json) = llm::extract_json(response) {
        if let Ok(mut req) = serde_json::from_value::<ContextRequest>(json) {
            // Enforce limits
            req.files.truncate(MAX_SCOUT_FILES);
            req.symbols.truncate(20);
            req.grep_patterns.truncate(MAX_SCOUT_PATTERNS);
            req.line_ranges.truncate(MAX_SCOUT_FILES);
            return Ok(req);
        }
    }

    // Fallback: empty request (compile pass will still work, just with less context)
    tracing::warn!("Scout response was not valid JSON, using empty context request");
    Ok(ContextRequest {
        files: Vec::new(),
        symbols: Vec::new(),
        grep_patterns: Vec::new(),
        line_ranges: Vec::new(),
        reasoning: "Scout produced invalid response; proceeding with existing context".to_string(),
    })
}

// ---------------------------------------------------------------------------
// Pass 2: Compile — fulfill ContextRequest using tools (no LLM)
// ---------------------------------------------------------------------------

async fn compile_context(
    request: &ContextRequest,
    tools: &ToolRegistry,
    docker: &DockerSession,
    executor: &Executor,
    confirmer: &dyn Confirmer,
    status_tx: Option<&StatusSender>,
    trace: Option<&ActiveTrace>,
) -> Result<CompiledContext> {
    let mut file_snippets: Vec<(String, String)> = Vec::new();
    let mut grep_results: Vec<(String, String)> = Vec::new();
    let symbol_context = String::new();
    let mut total_chars = 0usize;

    // 1. Read requested files
    for file_path in &request.files {
        if total_chars >= MAX_COMPILED_CONTEXT_CHARS {
            break;
        }
        let json = serde_json::json!({
            "tool": "file_read",
            "params": { "path": file_path }
        });
        match executor
            .execute_tool("file_read", &json, tools, docker, confirmer, status_tx, trace)
            .await
        {
            Ok(content) => {
                let truncated = if content.len() > 4000 {
                    format!("{}...[truncated]", &content[..content.floor_char_boundary(4000)])
                } else {
                    content
                };
                total_chars += truncated.len();
                file_snippets.push((file_path.clone(), truncated));
            }
            Err(e) => {
                tracing::debug!(file = %file_path, error = %e, "Scout: failed to read file");
            }
        }
    }

    // 2. Read specific line ranges
    for range_spec in &request.line_ranges {
        if total_chars >= MAX_COMPILED_CONTEXT_CHARS {
            break;
        }
        if let Some((path, start, end)) = parse_line_range(range_spec) {
            let json = serde_json::json!({
                "tool": "file_read",
                "params": {
                    "path": path,
                    "start_line": start,
                    "end_line": end
                }
            });
            match executor
                .execute_tool("file_read", &json, tools, docker, confirmer, status_tx, trace)
                .await
            {
                Ok(content) => {
                    total_chars += content.len();
                    file_snippets.push((format!("{}:{}-{}", path, start, end), content));
                }
                Err(e) => {
                    tracing::debug!(range = %range_spec, error = %e, "Scout: failed to read range");
                }
            }
        }
    }

    // 3. Execute grep patterns
    for pattern in &request.grep_patterns {
        if total_chars >= MAX_COMPILED_CONTEXT_CHARS {
            break;
        }
        let json = serde_json::json!({
            "tool": "grep",
            "params": {
                "pattern": pattern,
                "max_results": 20
            }
        });
        match executor
            .execute_tool("grep", &json, tools, docker, confirmer, status_tx, trace)
            .await
        {
            Ok(output) => {
                let truncated = if output.len() > 3000 {
                    format!("{}...[truncated]", &output[..output.floor_char_boundary(3000)])
                } else {
                    output
                };
                total_chars += truncated.len();
                grep_results.push((pattern.clone(), truncated));
            }
            Err(e) => {
                tracing::debug!(pattern = %pattern, error = %e, "Scout: grep failed");
            }
        }
    }

    Ok(CompiledContext {
        file_snippets,
        symbol_context,
        grep_results,
        total_chars,
    })
}

fn parse_line_range(spec: &str) -> Option<(&str, u32, u32)> {
    let colon_idx = spec.rfind(':')?;
    let path = &spec[..colon_idx];
    let range_part = &spec[colon_idx + 1..];
    let dash_idx = range_part.find('-')?;
    let start: u32 = range_part[..dash_idx].parse().ok()?;
    let end: u32 = range_part[dash_idx + 1..].parse().ok()?;
    Some((path, start, end))
}

/// Get a compact file tree using the codebase_map or glob tool.
async fn get_file_tree(
    tools: &ToolRegistry,
    docker: &DockerSession,
    executor: &Executor,
) -> String {
    // Try codebase_map first
    let json = serde_json::json!({
        "tool": "codebase_map",
        "params": {}
    });
    let confirmer = crate::confirm::AutoConfirmer;
    if let Ok(tree) = executor
        .execute_tool("codebase_map", &json, tools, docker, &confirmer, None, None)
        .await
    {
        if !tree.is_empty() {
            // Truncate if too large for scout context
            return if tree.len() > 6000 {
                format!("{}...[truncated]", &tree[..tree.floor_char_boundary(6000)])
            } else {
                tree
            };
        }
    }

    // Fallback: glob for source files
    let json = serde_json::json!({
        "tool": "glob",
        "params": { "pattern": "**/*.rs" }
    });
    if let Ok(files) = executor
        .execute_tool("glob", &json, tools, docker, &confirmer, None, None)
        .await
    {
        return files;
    }

    "(file tree unavailable)".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_context_request_valid() {
        let json = r#"{"files": ["src/main.rs", "src/lib.rs"], "symbols": ["Manager"], "grep_patterns": ["fn handle"], "line_ranges": ["src/foo.rs:10-50"], "reasoning": "Need to see the entry point"}"#;
        let req = parse_context_request(json).unwrap();
        assert_eq!(req.files.len(), 2);
        assert_eq!(req.symbols.len(), 1);
        assert_eq!(req.grep_patterns.len(), 1);
        assert_eq!(req.line_ranges.len(), 1);
        assert_eq!(req.reasoning, "Need to see the entry point");
    }

    #[test]
    fn parse_context_request_fallback() {
        let req = parse_context_request("This is not JSON").unwrap();
        assert!(req.files.is_empty());
        assert!(req.reasoning.contains("invalid"));
    }

    #[test]
    fn parse_line_range_valid() {
        let result = parse_line_range("src/manager.rs:100-200");
        assert!(result.is_some());
        let (path, start, end) = result.unwrap();
        assert_eq!(path, "src/manager.rs");
        assert_eq!(start, 100);
        assert_eq!(end, 200);
    }

    #[test]
    fn parse_line_range_invalid() {
        assert!(parse_line_range("no_range_here").is_none());
        assert!(parse_line_range("file.rs:abc-def").is_none());
    }

    #[test]
    fn compiled_context_to_string() {
        let ctx = CompiledContext {
            file_snippets: vec![
                ("src/main.rs".to_string(), "fn main() {}".to_string()),
            ],
            symbol_context: "function main [src/main.rs:1]".to_string(),
            grep_results: vec![
                ("fn handle".to_string(), "src/manager.rs:100: pub async fn handle".to_string()),
            ],
            total_chars: 100,
        };
        let output = ctx.to_context_string();
        assert!(output.contains("RELEVANT FILES:"));
        assert!(output.contains("src/main.rs"));
        assert!(output.contains("SEARCH RESULTS:"));
        assert!(output.contains("fn handle"));
    }

    #[test]
    fn context_request_truncation() {
        let json = serde_json::json!({
            "files": (0..30).map(|i| format!("file_{}.rs", i)).collect::<Vec<_>>(),
            "symbols": ["Foo"],
            "grep_patterns": (0..20).map(|i| format!("pattern_{}", i)).collect::<Vec<_>>(),
            "reasoning": "test"
        });
        let req = parse_context_request(&json.to_string()).unwrap();
        assert!(req.files.len() <= MAX_SCOUT_FILES);
        assert!(req.grep_patterns.len() <= MAX_SCOUT_PATTERNS);
    }
}
