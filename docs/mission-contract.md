# Athena Mission Contract

Date: 2026-02-15

## Mission Statement

Athena autonomously delivers backlog work across products and continuously improves her own capability, while maintaining quality and safety.

## Mission Lanes

- `delivery`: product and feature backlog execution across repos/products.
- `self_improvement`: reliability/capability improvements in Athena itself.

Both lanes are measured using the same KPI framework.

## KPI Contract

- `Task Success Rate (X)`:
  - Definition: `tasks_succeeded / tasks_started`
  - Goal: maximize.
- `Verification Pass Rate (Y)`:
  - Definition: `verifications_passed / verifications_total`
  - Goal: maximize.
- `Rollback Rate (Z)`:
  - Definition: `rollbacks / tasks_succeeded`
  - Goal: minimize.
- `Mean Time To Fix (Recovery)`:
  - Definition: average seconds from a failed code-change event to next successful code-change event.
  - Goal: minimize.

Safety invariant:

- `0` critical safety incidents (secret leaks, destructive ops, policy bypass).

## Program Baseline (2026-02-16)

Reality check against mission:

- strong execution/eval plumbing exists, but optimizer loop is not implemented yet
- full `athena-core-v1` benchmark remains below gate in recent runs
- low-risk delivery KPI is currently below the phase-1 target

Estimated maturity by layer:

- agent execution: ~85%
- evaluation: ~65%
- failure logging/telemetry: ~70%
- self-improvement optimizer loop: ~10%
- end-to-end closed loop: ~45-55%

See detailed roadmap: `docs/self-improvement-roadmap.md`.

## Initial Targets

Phase 1 (stabilize):

- task success rate `>= 40%`
- verification pass rate `>= 75%`
- rollback rate `<= 15%`
- mean time to fix `<= 48h`

Phase 2 (scale):

- task success rate `>= 60%`
- verification pass rate `>= 85%`
- rollback rate `<= 8%`
- mean time to fix `<= 24h`

Phase 3 (mission-ready):

- task success rate `>= 75%`
- verification pass rate `>= 92%`
- rollback rate `<= 3%`
- mean time to fix `<= 8h`

## Near-Term Program Goals (Added)

1. Truthful benchmark gate:
   - keep smoke benchmarks for integration checks
   - add/expand real benchmark gates for quality decisions
2. Execution contract hardening:
   - normalized CLI wrapper contract across claude_code/codex/opencode
   - deterministic error taxonomy + retry/fallback policy
3. Artifact completeness:
   - every autonomous run must emit artifacts and learning memories
4. Self-Build supervised pipeline:
   - worktree patching + maintenance pack + critic + policy promotion
5. Self-improvement optimizer loop:
   - candidate generation, tournament evaluation, merge-best under policy

## Measurement Guardrails

- Track KPIs segmented by:
  - `lane` (`delivery` | `self_improvement`)
  - `repo` (product/repository label)
  - `risk_tier` (`low` | `medium` | `high`)
- Do not report only aggregate values; review by lane and risk tier.
- Keep explicit self-improvement capacity (recommended baseline: 20%) so self-improvement does not get starved by delivery.

## How To Track Now vs Evolution

Use `athena kpi`:

- Current state:
  - `athena kpi status --lane self_improvement --repo athena --risk medium`
- Persist snapshot:
  - `athena kpi snapshot --lane self_improvement --repo athena --risk medium`
- Snapshot history (evolution):
  - `athena kpi history --lane self_improvement --repo athena --limit 30`

Recommended cadence:

- at least daily snapshots per active lane/repo;
- always snapshot before and after major refactor/autonomy changes.

Operational recommendation:

- keep `self_improvement` capacity explicitly reserved (20% baseline)
- do not raise autonomy thresholds until delivery lane reaches phase-1 KPI targets

Tagged attribution source:

- KPI values are now derived from lane/risk-tagged autonomous task outcomes when available.
- Dispatch metadata can be set explicitly from CLI:
  - `athena dispatch --goal "<...>" --lane delivery --risk medium --repo athena`
- Background autonomous loops emit tagged outcomes under `self_improvement` by default.

## Langfuse Tracking

Yes, KPI evolution can be tracked in Langfuse.

- Command:
  - `athena kpi snapshot --lane self_improvement --repo athena --risk medium --langfuse`
- Export shape:
  - trace event `mission:kpi_snapshot`
  - tags include `mission`, `kpi`, lane, risk tier
  - output payload contains the full KPI snapshot values

This gives a timeline in SQLite (`kpi_snapshots`) and in Langfuse for external observability dashboards.
