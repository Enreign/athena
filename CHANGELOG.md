# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added

- OpenAI-compatible API endpoints (`/v1/models`, `/v1/chat/completions`) with auth, rate limits, and docs.
- Ghost auto-specialization based on KPI outcomes with stability thresholds and rollback behavior.
- Session review and explainability system with activity-log persistence.
- Telegram activity commands: `/review`, `/explain`, `/watch`, `/search`, `/alerts`.
- MCP ToolRegistry wiring with namespaced tools (`mcp:<server>:<tool>`) and allowlist controls.
- Prompt scanner at chat/autonomous intake with `flag_only`/`block` modes and allowlist overrides.
- Tool-call loop guard circuit breaker (`manager.loop_guard`) to stop repeated call loops.
- Adaptive pre-dispatch token/context budgeting for oversized task contracts.
- HNSW semantic memory index with exact-cosine fallback for small/early datasets.
- Eval CLI wiring and scenario library manifest support.

### Changed

- Autonomous task routing now evaluates historical KPI outcomes when selecting a default ghost.
- Code-index/scout overlap cleanup: indexing remains proactive, with overlapping paths removed.

## [0.1.0] - 2026-02-26

### Added

- initial public repository release metadata and policy docs
- tag-based GitHub release workflow
- deterministic profile toggle via `ATHENA_DISABLE_HOME_PROFILES`

### Changed

- `athena ghosts` no longer requires LLM connectivity
- `doctor --ci` now treats optional self-improvement loops as warnings when not enabled
- maintainability baseline refreshed to current code layout

### Removed

- tracked runtime logs and local scratch artifact from repository history going forward
