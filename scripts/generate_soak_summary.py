#!/usr/bin/env python3
"""
Generate a concise soak summary artifact from soak logs and latest eval outputs.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
from pathlib import Path
from typing import Any


STEP_RE = re.compile(r"step=([A-Za-z0-9_-]+) status=(PASS|FAIL)")
SUMMARY_RE = re.compile(r"summary pass_steps=(\d+) fail_steps=(\d+) iterations=(\d+)")
GATE_RE = re.compile(r"\bgate=(PASS|FAIL)\b")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate soak summary markdown.")
    parser.add_argument("--run-dir", required=True, help="soak run directory")
    parser.add_argument("--results-dir", default="eval/results", help="eval results directory")
    parser.add_argument("--out", default="", help="output markdown path")
    return parser.parse_args()


def parse_state(path: Path) -> dict[str, str]:
    if not path.exists():
        return {}
    out: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8", errors="ignore").splitlines():
        line = raw.strip()
        if not line or "=" not in line:
            continue
        key, value = line.split("=", 1)
        out[key.strip()] = value.strip()
    return out


def resolve_path(repo: Path, raw: str) -> Path:
    path = Path(raw).expanduser()
    if path.is_absolute():
        return path
    return (repo / path).resolve()


def find_last_value(lines: list[str], marker: str) -> str:
    for line in reversed(lines):
        idx = line.find(marker)
        if idx == -1:
            continue
        return line[idx + len(marker) :].strip()
    return ""


def main() -> int:
    args = parse_args()
    repo = Path.cwd().resolve()
    run_dir = resolve_path(repo, args.run_dir)
    results_dir = resolve_path(repo, args.results_dir)
    out_path = resolve_path(repo, args.out) if args.out else run_dir / "summary.md"

    log_path = run_dir / "soak.log"
    if not log_path.exists():
        raise SystemExit(f"missing soak log: {log_path}")

    lines = log_path.read_text(encoding="utf-8", errors="ignore").splitlines()
    state = parse_state(run_dir / "state.env")
    generated_utc = dt.datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%SZ")

    step_stats: dict[str, dict[str, int]] = {}
    gate_pass = 0
    gate_fail = 0
    for line in lines:
        step_match = STEP_RE.search(line)
        if step_match:
            step = step_match.group(1)
            status = step_match.group(2).lower()
            bucket = step_stats.setdefault(step, {"pass": 0, "fail": 0})
            bucket[status] += 1
        gate_match = GATE_RE.search(line)
        if gate_match:
            if gate_match.group(1) == "PASS":
                gate_pass += 1
            else:
                gate_fail += 1

    iterations = sum(1 for line in lines if line.startswith("==== ITERATION"))
    pass_steps = sum(v["pass"] for v in step_stats.values())
    fail_steps = sum(v["fail"] for v in step_stats.values())

    for line in reversed(lines):
        sm = SUMMARY_RE.search(line)
        if sm:
            pass_steps = int(sm.group(1))
            fail_steps = int(sm.group(2))
            iterations = int(sm.group(3))
            break

    matrix_json_raw = find_last_value(lines, "matrix_json=")
    dashboard_raw = find_last_value(lines, "dashboard=")
    backlog_json_raw = find_last_value(lines, "backlog_latest_json=")
    backlog_md_raw = find_last_value(lines, "backlog_latest_md=")

    matrix_json = resolve_path(repo, matrix_json_raw) if matrix_json_raw else None
    dashboard_md = resolve_path(repo, dashboard_raw) if dashboard_raw else None
    backlog_json = resolve_path(repo, backlog_json_raw) if backlog_json_raw else (results_dir / "improvement-backlog-latest.json")
    backlog_md = resolve_path(repo, backlog_md_raw) if backlog_md_raw else (results_dir / "improvement-backlog-latest.md")

    top_tickets: list[dict[str, Any]] = []
    if backlog_json.exists():
        try:
            payload = json.loads(backlog_json.read_text(encoding="utf-8", errors="ignore"))
            top_tickets = list(payload.get("tickets", []))[:5]
        except json.JSONDecodeError:
            top_tickets = []

    lines_out: list[str] = []
    lines_out.append("# Soak Summary")
    lines_out.append("")
    lines_out.append(f"- generated_utc: {generated_utc}")
    lines_out.append(f"- run_dir: `{run_dir}`")
    lines_out.append(f"- soak_name: `{state.get('SOAK_NAME', 'unknown')}`")
    lines_out.append(f"- start_ts: `{state.get('START_TS', 'unknown')}`")
    lines_out.append(f"- configured_duration_secs: `{state.get('DURATION_SECS', 'unknown')}`")
    lines_out.append(f"- configured_interval_secs: `{state.get('INTERVAL_SECS', 'unknown')}`")
    lines_out.append("")
    lines_out.append("## Runtime Totals")
    lines_out.append("")
    lines_out.append(f"- iterations: `{iterations}`")
    lines_out.append(f"- step_pass: `{pass_steps}`")
    lines_out.append(f"- step_fail: `{fail_steps}`")
    lines_out.append(f"- matrix_gate_pass_events: `{gate_pass}`")
    lines_out.append(f"- matrix_gate_fail_events: `{gate_fail}`")
    lines_out.append("")
    lines_out.append("## Step Breakdown")
    lines_out.append("")
    lines_out.append("| step | pass | fail |")
    lines_out.append("|---|---:|---:|")
    for step in sorted(step_stats):
        lines_out.append(f"| `{step}` | {step_stats[step]['pass']} | {step_stats[step]['fail']} |")
    lines_out.append("")
    lines_out.append("## Latest Artifacts")
    lines_out.append("")
    if matrix_json is not None:
        lines_out.append(f"- matrix_json: `{matrix_json}`")
    if dashboard_md is not None:
        lines_out.append(f"- dashboard_md: `{dashboard_md}`")
    if backlog_json is not None:
        lines_out.append(f"- backlog_json: `{backlog_json}`")
    if backlog_md is not None:
        lines_out.append(f"- backlog_md: `{backlog_md}`")
    lines_out.append("")

    if top_tickets:
        lines_out.append("## Top Improvement Tickets")
        lines_out.append("")
        rank = 1
        for ticket in top_tickets:
            score = ticket.get("score", 0)
            source = ticket.get("source", "unknown")
            risk = ticket.get("risk", "unknown")
            title = ticket.get("title", "(untitled)")
            lines_out.append(f"{rank}. [{score:.3f}] `{source}` `{risk}` {title}")
            rank += 1
        lines_out.append("")

    lines_out.append("## Next Actions")
    lines_out.append("")
    if gate_fail > 0:
        lines_out.append("1. Inspect latest matrix report and address recurring gate failures before promotion.")
    else:
        lines_out.append("1. Keep matrix green and increase benchmark depth from smoke to real-task suites.")
    if top_tickets:
        lines_out.append("2. Execute the top-ranked backlog ticket in supervised mode and verify with benchmark rerun.")
    else:
        lines_out.append("2. Generate additional failure signals if backlog is empty (run non-smoke suite).")
    lines_out.append("3. Re-run KPI snapshot and compare against mission thresholds before raising autonomy.")
    lines_out.append("")

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(lines_out), encoding="utf-8")
    print(f"soak_summary_md={out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
