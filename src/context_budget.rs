//! Adaptive Context Budgeting
//!
//! Gates context assembly on classification complexity — SIMPLE tasks skip
//! expensive context sources (KPI, lessons, memory search), while COMPLEX
//! tasks get the full context pack. This avoids wasting tokens on context
//! that won't affect the response.

use std::time::Instant;

/// Classification complexity tier — determines which context sources to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextTier {
    /// Greetings, knowledge questions, status queries.
    /// Skip: KPI, lessons, memory search, metrics.
    /// Keep: persona, mood, relationship (personality-relevant).
    Minimal,
    /// Direct tool execution (git status, deploy, etc.)
    /// Skip: conversation summary, relationship, mood.
    /// Keep: tool usage stats, recent turns (for param context).
    ToolFocused,
    /// Ghost-delegated tasks: full context, weighted by ghost role.
    Full,
}

/// Specifies which context sources to assemble for a given request.
#[derive(Debug, Clone)]
pub struct ContextBudgetPlan {
    pub tier: ContextTier,
    pub load_memories: bool,
    pub load_kpi: bool,
    pub load_lessons: bool,
    pub load_metrics: bool,
    pub load_mood: bool,
    pub load_relationship: bool,
    pub load_user_profile: bool,
    pub load_routing_context: bool,
    pub memory_limit: usize,
    pub recent_turns_limit: usize,
}

impl ContextBudgetPlan {
    /// Full context plan — used for COMPLEX classifications.
    pub fn full() -> Self {
        Self {
            tier: ContextTier::Full,
            load_memories: true,
            load_kpi: true,
            load_lessons: true,
            load_metrics: true,
            load_mood: true,
            load_relationship: true,
            load_user_profile: true,
            load_routing_context: true,
            memory_limit: 10,
            recent_turns_limit: 20,
        }
    }

    /// Minimal context — used for SIMPLE classifications.
    pub fn minimal() -> Self {
        Self {
            tier: ContextTier::Minimal,
            load_memories: false,
            load_kpi: false,
            load_lessons: false,
            load_metrics: false,
            load_mood: true,
            load_relationship: true,
            load_user_profile: true,
            load_routing_context: false,
            memory_limit: 0,
            recent_turns_limit: 8,
        }
    }

    /// Tool-focused context — used for DIRECT classifications.
    pub fn tool_focused() -> Self {
        Self {
            tier: ContextTier::ToolFocused,
            load_memories: false,
            load_kpi: false,
            load_lessons: false,
            load_metrics: true,
            load_mood: false,
            load_relationship: false,
            load_user_profile: true,
            load_routing_context: false,
            memory_limit: 3,
            recent_turns_limit: 10,
        }
    }
}

/// Pre-classification budget: lightweight context for the classifier itself.
/// The classifier needs enough context to decide, but doesn't need KPI history
/// or full memory search for simple requests.
#[derive(Debug, Clone)]
pub struct ClassifierContextPlan {
    pub recent_turns_limit: usize,
    pub memory_limit: usize,
    pub load_kpi: bool,
    pub load_lessons: bool,
}

impl Default for ClassifierContextPlan {
    fn default() -> Self {
        Self {
            recent_turns_limit: 20,
            memory_limit: 10,
            load_kpi: true,
            load_lessons: true,
        }
    }
}

/// Infer a lightweight classifier plan from input length and recency.
/// Short, simple inputs get cheaper classifier context.
pub fn infer_classifier_plan(user_input: &str, turn_count: usize) -> ClassifierContextPlan {
    let input_len = user_input.len();
    let has_code_keywords = contains_code_keywords(user_input);

    if input_len < 40 && !has_code_keywords && turn_count < 5 {
        // Very short input, no code keywords, early in conversation
        ClassifierContextPlan {
            recent_turns_limit: 6,
            memory_limit: 3,
            load_kpi: false,
            load_lessons: false,
        }
    } else if !has_code_keywords {
        // Longer input but no code keywords
        ClassifierContextPlan {
            recent_turns_limit: 12,
            memory_limit: 5,
            load_kpi: false,
            load_lessons: true,
        }
    } else {
        ClassifierContextPlan::default()
    }
}

/// Check if user input contains keywords suggesting code/tool operations.
fn contains_code_keywords(input: &str) -> bool {
    let lower = input.to_lowercase();
    const KEYWORDS: &[&str] = &[
        "code",
        "implement",
        "refactor",
        "fix",
        "bug",
        "build",
        "deploy",
        "test",
        "write",
        "edit",
        "modify",
        "create",
        "delete",
        "git",
        "cargo",
        "npm",
        "docker",
        "file",
        "function",
        "struct",
        "class",
        "module",
        "import",
        "compile",
        "lint",
        "pr",
        "merge",
        "branch",
        "commit",
        "push",
        "pull",
    ];
    KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// Track context assembly timings for observability.
#[derive(Debug, Clone)]
pub struct ContextAssemblyMetrics {
    pub tier: ContextTier,
    pub memory_search_ms: Option<u128>,
    pub embedding_ms: Option<u128>,
    pub kpi_ms: Option<u128>,
    pub total_ms: u128,
    pub sources_loaded: usize,
    pub sources_skipped: usize,
}

impl ContextAssemblyMetrics {
    pub fn start(tier: ContextTier) -> ContextAssemblyTimer {
        ContextAssemblyTimer {
            tier,
            started: Instant::now(),
            memory_search_ms: None,
            embedding_ms: None,
            kpi_ms: None,
            sources_loaded: 0,
            sources_skipped: 0,
        }
    }
}

/// Builder for assembling context metrics during the context loading phase.
pub struct ContextAssemblyTimer {
    tier: ContextTier,
    started: Instant,
    memory_search_ms: Option<u128>,
    embedding_ms: Option<u128>,
    kpi_ms: Option<u128>,
    sources_loaded: usize,
    sources_skipped: usize,
}

impl ContextAssemblyTimer {
    pub fn record_memory_search(&mut self, ms: u128) {
        self.memory_search_ms = Some(ms);
    }

    pub fn record_embedding(&mut self, ms: u128) {
        self.embedding_ms = Some(ms);
    }

    pub fn record_kpi(&mut self, ms: u128) {
        self.kpi_ms = Some(ms);
    }

    pub fn record_loaded(&mut self) {
        self.sources_loaded += 1;
    }

    pub fn record_skipped(&mut self) {
        self.sources_skipped += 1;
    }

    pub fn finish(self) -> ContextAssemblyMetrics {
        ContextAssemblyMetrics {
            tier: self.tier,
            memory_search_ms: self.memory_search_ms,
            embedding_ms: self.embedding_ms,
            kpi_ms: self.kpi_ms,
            total_ms: self.started.elapsed().as_millis(),
            sources_loaded: self.sources_loaded,
            sources_skipped: self.sources_skipped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_plan_skips_expensive_sources() {
        let plan = ContextBudgetPlan::minimal();
        assert!(!plan.load_memories);
        assert!(!plan.load_kpi);
        assert!(!plan.load_lessons);
        assert!(!plan.load_metrics);
        // But keeps personality-relevant sources
        assert!(plan.load_mood);
        assert!(plan.load_relationship);
    }

    #[test]
    fn tool_focused_plan_skips_personality() {
        let plan = ContextBudgetPlan::tool_focused();
        assert!(!plan.load_mood);
        assert!(!plan.load_relationship);
        // But keeps tool stats
        assert!(plan.load_metrics);
    }

    #[test]
    fn full_plan_loads_everything() {
        let plan = ContextBudgetPlan::full();
        assert!(plan.load_memories);
        assert!(plan.load_kpi);
        assert!(plan.load_lessons);
        assert!(plan.load_metrics);
        assert!(plan.load_mood);
        assert!(plan.load_relationship);
    }

    #[test]
    fn classifier_plan_cheap_for_short_input() {
        let plan = infer_classifier_plan("hello", 2);
        assert!(!plan.load_kpi);
        assert!(!plan.load_lessons);
        assert_eq!(plan.memory_limit, 3);
    }

    #[test]
    fn classifier_plan_full_for_code_input() {
        let plan = infer_classifier_plan("implement a new auth module", 2);
        assert!(plan.load_kpi);
        assert!(plan.load_lessons);
        assert_eq!(plan.memory_limit, 10);
    }

    #[test]
    fn code_keywords_detected() {
        assert!(contains_code_keywords("fix the login bug"));
        assert!(contains_code_keywords("git push origin main"));
        assert!(contains_code_keywords("Implement new feature"));
        assert!(!contains_code_keywords("hello how are you"));
        assert!(!contains_code_keywords("what's the weather"));
    }

    #[test]
    fn context_assembly_timer_tracks_metrics() {
        let mut timer = ContextAssemblyMetrics::start(ContextTier::Full);
        timer.record_memory_search(50);
        timer.record_embedding(30);
        timer.record_loaded();
        timer.record_loaded();
        timer.record_skipped();
        let metrics = timer.finish();
        assert_eq!(metrics.tier, ContextTier::Full);
        assert_eq!(metrics.memory_search_ms, Some(50));
        assert_eq!(metrics.embedding_ms, Some(30));
        assert_eq!(metrics.sources_loaded, 2);
        assert_eq!(metrics.sources_skipped, 1);
    }
}
