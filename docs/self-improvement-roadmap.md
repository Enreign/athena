# Athena Self-Improvement Roadmap

Date: 2026-02-16

## Current State (Baseline)

As of 2026-02-16, Athena has strong execution/eval plumbing but does not yet implement a true self-improvement optimizer loop (OpenEvolve-style prompt/skill evolution and selection).

### Implemented Foundations

- Multi-CLI coding execution (`claude_code`, `codex`, `opencode`)
- Autonomous task dispatch, outcomes, and memory logging
- Metrics collection and anomaly-oriented diagnostics wiring
- Refactoring and pattern scanners
- Eval harness, matrix runs, trend history, dashboard, CI smoke

### Key Gaps vs Vision

- No prompt/skill mutation + selection tournament loop
- No automatic versioned prompt/skill rewriting from benchmark outcomes
- No merge-best candidate pipeline across fixed benchmark tasks
- CI gate is smoke-only (mock dispatch), not full real benchmark
- CLI smoke benchmark is lightweight and format-driven
- Benchmark fast mode skips EXPLORE/VERIFY for speed and should not be treated as full-quality proof

### Maturity Estimate (2026-02-16)

- Agent execution layer: ~85%
- Evaluation layer: ~65%
- Failure logging/telemetry layer: ~70%
- Self-improvement optimizer: ~10%
- End-to-end closed loop (execute -> evaluate -> evolve -> promote): ~45-55%

## Mission Program Goals (Updated)

1. Reach reliable delivery baseline:
   - low-risk delivery task success rate >= 40%
   - keep verification pass rate >= 75%
   - keep rollback rate <= 15%
2. Harden execution contract:
   - one normalized contract across all coding CLIs
   - deterministic error taxonomy and policy-driven retry/fallback
3. Guarantee learning artifacts:
   - every autonomous run emits required artifacts + memory entries
4. Build Self-Build pipeline (supervised first):
   - detect -> patch in isolated worktree -> maintenance pack -> critic -> promote
5. Implement optimizer loop:
   - generate candidates, evaluate on fixed set, select/persist best, promote by policy

## Workstreams and Exit Criteria

### WS1 — Eval Gate Hardening

- Expand full benchmark suite (real tasks, stricter acceptance, non-format-only)
- Add real benchmark CI job (in addition to smoke)
- Track lane/risk/repo segmented trends

Exit criteria:
- 14 consecutive scheduled runs without unresolved `started` outcomes
- CI includes at least one real benchmark gate on non-mock runtime

### WS2 — CLI Execution Contract Hardening

- Standardize CLI adapter contract (inputs, outputs, status, error codes, artifacts)
- Add deterministic error taxonomy
- Add policy matrix for retry/fallback by error code

Exit criteria:
- Contract tests pass for all three CLIs
- Same error input produces same policy decision (deterministic replay)

### WS3 — Reliable Autonomous Learning Output

- Enforce per-run artifact bundle (request, plan, transcript, diff summary, verify summary, outcome)
- Enforce per-run memory entries (success/failure, root cause, follow-up)

Exit criteria:
- 100% of terminal autonomous runs include full artifact + memory set

### WS4 — Self-Build Supervised Pipeline

- Implement isolated-worktree patch pipeline with maintenance pack
- Add promotion rules:
  - auto-merge only low-risk + high-confidence + green gates
  - medium/high risk always PR-only
- Enforce guardrails: no secret access, no destructive git ops, no direct main edits

Exit criteria:
- 20 supervised self-build runs, zero guardrail violations

### WS5 — Self-Improvement Backlog + Optimizer Loop

- Auto-generate ranked improvement tickets from failures + maintainability hotspots
- Prioritize by expected impact and confidence
- Run candidate prompt/skill policy tournaments on fixed benchmark set
- Persist best candidates with provenance

Exit criteria:
- Daily ranked backlog generated automatically
- At least one optimizer-selected candidate promoted under policy without regressions

## Recommended Execution Order

1. Eval harness hardening and truthful gate signal
2. CLI execution contract + deterministic error policy
3. Self-Build pipeline in supervised mode
4. Self-improvement backlog + optimizer loop
5. Raise autonomy thresholds gradually (policy-gated)

## Governance Rules

- Never bypass guardrails for speed
- Benchmark fast mode is for integration smoke only, not release-quality proof
- Promotion and autonomy thresholds must be tied to measured KPI/eval trends

## Next 8-9h Execution Plan (Operator Runbook)

### Phase A (Hour 0-1): Stabilize Signal

1. Ensure one reachable LLM backend per configured CLI (claude_code/codex/opencode).
2. Run `athena doctor` and confirm all funnels are green except known benchmark-quality gaps.
3. Start detached soak:
   - `./scripts/start-soak-autonomy.sh 28800 1800 overnight8h`

Expected artifacts:

- `eval/results/soak-overnight8h-<timestamp>/soak.log`
- `eval/results/soak-overnight8h-<timestamp>/summary.md` (on completion)

### Phase B (Hour 1-7): Reliability + Backlog Feed

Every soak iteration should produce:

- doctor result
- 3-CLI smoke matrix
- KPI snapshots (`delivery`, `self_improvement`)
- dashboard refresh
- current maintainability metrics snapshot
- ranked improvement backlog (`improvement-backlog-latest.*`)

Monitoring objective:

- no stuck/dangling runs
- deterministic terminal outcomes
- stable CLI matrix pass trend

### Phase C (Hour 7-9): Morning Handoff

1. Review soak summary + latest matrix/dashboard/backlog artifacts.
2. Pick top 1-2 backlog tickets by score and confidence.
3. Execute one supervised improvement ticket with benchmark rerun.
4. Update mission KPI snapshot and compare against phase-1 thresholds.

Success criteria for this window:

- soak completes without launcher/runtime crashes
- backlog is generated automatically from live failure + maintainability signals
- summary artifact exists with ranked next actions for immediate follow-up
