#!/usr/bin/env python3
"""Fail-closed read-only scope registration for normal UI acceptance."""
from __future__ import annotations
import argparse
import json
import sqlite3
import sys
import time
from pathlib import Path

def fail(code: str) -> None:
    print(json.dumps({"ok": False, "code": code}, ensure_ascii=True))
    raise SystemExit(0)

def read_once(database: Path, novel: str, chapter: str, content_hash: str) -> dict | None:
    if not database.is_file():
        raise RuntimeError("UI_SCOPE_DB_MISSING")
    conn = sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True, isolation_level=None)
    conn.row_factory = sqlite3.Row
    try:
        conn.execute("PRAGMA query_only=ON")
        conn.execute("BEGIN")
        rows = conn.execute(
            """
            SELECT w.project_id AS project_id, w.id AS work_id, c.id AS chapter_id,
                   r.id AS revision_id, j.id AS job_id, j.attempt_no AS job_attempt_no,
                   s.id AS source_run_id, attempt.attempt_no AS source_attempt_no
            FROM novel_works w
            JOIN novel_chapters c ON c.novel_work_id=w.id
            JOIN novel_chapter_revisions r ON r.id=c.current_revision_id
            JOIN novel_production_jobs j ON j.project_id=w.project_id
              AND j.novel_work_id=w.id AND j.novel_chapter_id=c.id
              AND j.source_revision_id=r.id
            JOIN source_analysis_runs s ON s.novel_chapter_revision_id=r.id
            JOIN novel_analysis_lineages lineage ON lineage.id=s.novel_analysis_lineage_id
            JOIN source_analysis_run_attempts attempt ON attempt.source_analysis_run_id=s.id
            WHERE w.title=:novel AND c.title=:chapter AND r.content_hash=:content_hash
              AND j.status='running' AND j.stage='source_analysis'
              AND s.status='running'
              AND lineage.novel_work_id=w.id
              AND s.frozen_comic_adaptation_id=j.default_adaptation_id
              AND s.idempotency_key=j.id || ':source:' || j.attempt_no
              AND s.created_at>=j.created_at
              AND attempt.status='running' AND attempt.lease_owner IS NOT NULL
              AND attempt.lease_expires_at>=CAST(unixepoch('now') * 1000 AS INTEGER)
              AND (j.source_analysis_run_id IS NULL OR j.source_analysis_run_id=s.id)
            """,
            {"novel": novel, "chapter": chapter, "content_hash": content_hash},
        ).fetchall()
        if len(rows) == 1:
            row = rows[0]
            return {key: row[key] for key in ("project_id", "work_id", "chapter_id", "revision_id", "job_id", "job_attempt_no", "source_run_id", "source_attempt_no")}
        if len(rows) > 1:
            raise RuntimeError("UI_SCOPE_NOT_UNIQUE")
        return None
    finally:
        conn.close()

def main() -> None:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--database", required=True)
    parser.add_argument("--novel", required=True)
    parser.add_argument("--chapter", required=True)
    parser.add_argument("--content-hash", required=True)
    parser.add_argument("--wait-ms", type=int, default=30000)
    args = parser.parse_args()
    if args.wait_ms < 1 or args.wait_ms > 30000:
        fail("UI_SCOPE_WAIT_INVALID")
    deadline = time.monotonic() + args.wait_ms / 1000
    try:
        while True:
            value = read_once(Path(args.database), args.novel, args.chapter, args.content_hash)
            if value:
                print(json.dumps({"ok": True, "scope": value}, ensure_ascii=True))
                return
            if time.monotonic() >= deadline:
                fail("UI_SCOPE_NOT_READY")
            time.sleep(0.25)
    except (OSError, sqlite3.Error, RuntimeError) as error:
        fail(str(error) if str(error) else "UI_SCOPE_READ_FAILED")

if __name__ == "__main__":
    main()
