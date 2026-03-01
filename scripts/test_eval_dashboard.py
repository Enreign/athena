#!/usr/bin/env python3
import sqlite3
import sys
import unittest
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import eval_dashboard


def _setup_conn() -> sqlite3.Connection:
    conn = sqlite3.connect(":memory:")
    conn.execute(
        """
        CREATE TABLE kpi_snapshots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            lane TEXT NOT NULL,
            repo TEXT NOT NULL,
            risk_tier TEXT NOT NULL,
            captured_at TEXT NOT NULL,
            task_success_rate REAL NOT NULL,
            verification_pass_rate REAL NOT NULL,
            rollback_rate REAL NOT NULL,
            mean_time_to_fix_secs REAL,
            tasks_started INTEGER NOT NULL,
            tasks_succeeded INTEGER NOT NULL,
            tasks_failed INTEGER NOT NULL,
            verifications_total INTEGER NOT NULL,
            verifications_passed INTEGER NOT NULL,
            rollbacks INTEGER NOT NULL
        )
        """
    )
    return conn


class EvalDashboardTests(unittest.TestCase):
    def test_render_dashboard_handles_missing_kpi_trend(self) -> None:
        content = eval_dashboard.render_dashboard(
            history=[],
            kpis=[],
            kpi_trend=[],
            repo_name="athena",
            lane_filter="delivery",
            risk_filter="high",
        )
        self.assertIn("no KPI snapshot trend found for current filters", content)
        self.assertIn("| - | - | - | n/a | n/a | n/a | 0 |", content)
        self.assertIn("- lane_filter: `delivery`", content)
        self.assertIn("- risk_filter: `high`", content)

    def test_query_kpi_snapshot_trend_filters_and_orders(self) -> None:
        conn = _setup_conn()
        rows = [
            ("delivery", "athena", "low", "2026-03-01 01:00:00", 0.50, 0.60, 0.10, 10, 5, 5),
            ("delivery", "athena", "low", "2026-03-01 03:00:00", 0.70, 0.80, 0.05, 12, 9, 3),
            ("delivery", "athena", "medium", "2026-03-01 02:00:00", 0.55, 0.65, 0.12, 8, 4, 4),
            ("self_improvement", "athena", "low", "2026-03-01 02:00:00", 0.90, 0.95, 0.01, 6, 6, 0),
            ("delivery", "other", "low", "2026-03-01 02:00:00", 0.33, 0.40, 0.25, 3, 1, 2),
        ]
        conn.executemany(
            """
            INSERT INTO kpi_snapshots (
                lane, repo, risk_tier, captured_at,
                task_success_rate, verification_pass_rate, rollback_rate,
                mean_time_to_fix_secs,
                tasks_started, tasks_succeeded, tasks_failed,
                verifications_total, verifications_passed, rollbacks
            ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, 0, 0, 0)
            """,
            rows,
        )
        conn.commit()

        trend = eval_dashboard.query_kpi_snapshot_trend(
            conn,
            repo_name="athena",
            lane_filter="delivery",
            risk_filter="low",
            limit=20,
        )
        self.assertEqual(len(trend), 2)
        self.assertEqual(trend[0]["captured_at"], "2026-03-01 01:00:00")
        self.assertEqual(trend[1]["captured_at"], "2026-03-01 03:00:00")

        content = eval_dashboard.render_dashboard(
            history=[],
            kpis=[],
            kpi_trend=trend,
            repo_name="athena",
            lane_filter="delivery",
            risk_filter="low",
        )
        self.assertIn("50.0% -> 70.0% (up", content)
        self.assertIn("| `2026-03-01 03:00:00` | `delivery` | `low` | 70.0% | 80.0% | 5.0% | 12 |", content)


if __name__ == "__main__":
    unittest.main()
