# LNY-30 Epic Rollout Plan

## Scope

This rollout splits into three tracks with conservative defaults and explicit gates:

- Track A: `athena eval` first-class CLI command
- Track B: KPI-driven ghost auto-specialization
- Track C: OpenAI-compatible API (`/v1/models`, `/v1/chat/completions`)

## Guarded Rollout

1. Track A ships first and is the quality gate for B and C.
2. Track B uses conservative evidence thresholds and defaults to fallback ghost when weak.
3. Track C ships with narrow request support and explicit rejection for unsupported options.

## Track A: `athena eval`

### Acceptance tests

1. `athena eval --suite eval/benchmark-cli-smoke.json --max-tasks 1` exits `0` on passing gate.
2. The command writes a normalized machine-readable artifact with:
- run id
- suite name
- scenario version
- provenance metadata (suite path + git commit)
- gate outcome and reasons
3. Baseline check fails non-zero when baseline evidence is required and missing.
4. Baseline regression check fails non-zero when score regresses beyond allowed delta.
5. `--update-baseline` writes/refreshes baseline artifact after successful run.

### Observability requirements

- Log fields: `run_id`, `suite`, `suite_version`, `baseline_version`, `gate_ok`, `baseline_ok`, `reasons`.
- Persist eval summary for KPI traceability in DB (`eval_runs`).

## Track B: Ghost auto-specialization

### Acceptance tests

1. When KPI evidence is strong (samples + confidence), autonomous execution selects the top ghost deterministically.
2. When evidence is weak/insufficient, selection falls back to default ghost.
3. Explicitly requested ghost bypasses specialization.
4. Selected ghost is persisted in autonomous outcome rows and contributes to future KPI queries.

### Observability requirements

- Log fields: `lane`, `repo`, `risk`, `selected_ghost`, `selection_mode`, `sample_count`, `success_rate`, `confidence_gap`, `rationale`.
- Emit specialization decision log before task execution.

## Track C: OpenAI-compatible API

### Acceptance tests

1. `GET /v1/models` returns OpenAI-compatible list shape and includes Athena model/ghost identities.
2. `POST /v1/chat/completions` accepts minimal fields (`model`, `messages`, `user`) and returns OpenAI-like response object.
3. Unsupported options (for example `stream=true`, tools/function-calling fields) return explicit `400` with structured error JSON.
4. Missing/invalid bearer token returns `401` when auth is enabled.
5. Request timeout returns `504` with structured error JSON.

### Observability requirements

- Log request class and outcome for all API entrypoints.
- Log fields for chat: request id, model, session id, tool count observed, status.
- Preserve policy boundaries by using non-interactive confirmer behavior (no implicit unsafe approvals).

## Initial Rollout Defaults

- Track A: baseline evidence required by default (`--allow-missing-baseline` opt-out).
- Track B: conservative minimum sample + confidence gap thresholds; fallback to default ghost.
- Track C: auth enabled by default via env-backed bearer token; narrow payload support only.
