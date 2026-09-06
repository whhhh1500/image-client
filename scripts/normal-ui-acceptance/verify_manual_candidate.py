#!/usr/bin/env python3
"""Read-only invariant verifier for one manual world-facts candidate edit."""
from __future__ import annotations
import argparse, json, sqlite3, sys
from pathlib import Path

def fail(code: str) -> None:
    print(json.dumps({"ok": False, "code": code}, ensure_ascii=True)); raise SystemExit(0)
def db(path: str) -> sqlite3.Connection:
    conn=sqlite3.connect(Path(path).resolve().as_uri()+"?mode=ro",uri=True,isolation_level=None); conn.row_factory=sqlite3.Row
    conn.execute("PRAGMA query_only=ON"); conn.execute("BEGIN"); return conn
def snapshot(conn: sqlite3.Connection, scope: dict) -> dict:
    artifact=conn.execute("""SELECT a.id,a.adopted_head_revision_id,a.candidate_head_revision_id,a.status,a.optimistic_version
      FROM analysis_artifacts a WHERE a.source_analysis_run_id=:sourceAnalysisRunId
      AND a.novel_work_id=:novelWorkId AND a.novel_chapter_revision_id=:sourceRevisionId
      AND a.artifact_type='world_facts'""",scope).fetchall()
    if len(artifact)!=1: raise RuntimeError("UI_MANUAL_WORLD_ARTIFACT_NOT_UNIQUE")
    art=artifact[0]
    if not art["adopted_head_revision_id"]: raise RuntimeError("UI_MANUAL_WORLD_ADOPTED_MISSING")
    if not art["candidate_head_revision_id"]: raise RuntimeError("UI_MANUAL_WORLD_CANDIDATE_HEAD_MISSING")
    candidates=conn.execute("SELECT id,parent_revision_id FROM analysis_artifact_revisions WHERE analysis_artifact_id=? AND status='candidate' ORDER BY id",(art["id"],)).fetchall()
    rows=conn.execute("""SELECT m.id member_id,m.status member_status,r.id run_id,r.status run_status,r.asset_id
      FROM comic_visual_batches b JOIN comic_visual_batch_members m ON m.batch_id=b.id
      JOIN comic_visual_page_runs r ON r.id=m.page_run_id
      WHERE b.production_job_id=:productionJobId AND b.project_id=:projectId AND b.novel_work_id=:novelWorkId""",scope).fetchall()
    if len(rows)!=1 or not rows[0]["asset_id"]: raise RuntimeError("UI_MANUAL_VISUAL_NOT_UNIQUE")
    return {"artifactId":art["id"],"adoptedRevisionId":art["adopted_head_revision_id"],"candidateHeadRevisionId":art["candidate_head_revision_id"],"artifactStatus":art["status"],"optimisticVersion":art["optimistic_version"],"candidateCount":len(candidates),"candidateIds":[r["id"] for r in candidates],"candidateParents":{r["id"]:r["parent_revision_id"] for r in candidates},"memberId":rows[0]["member_id"],"memberStatus":rows[0]["member_status"],"pageRunId":rows[0]["run_id"],"pageRunStatus":rows[0]["run_status"],"assetId":rows[0]["asset_id"]}
def main() -> None:
    p=argparse.ArgumentParser(add_help=False); p.add_argument("--database",required=True);p.add_argument("--scope",required=True);p.add_argument("--before");args=p.parse_args()
    try:
      scope=json.loads(Path(args.scope).read_text(encoding="utf-8"))
      required={"projectId","novelWorkId","novelChapterId","sourceRevisionId","productionJobId","sourceAnalysisRunId"}
      if not isinstance(scope,dict) or not required.issubset(scope) or any(not isinstance(scope[key],str) or not scope[key] for key in required): fail("UI_MANUAL_SCOPE_INVALID")
      c=db(args.database)
      try: now=snapshot(c,scope)
      finally: c.close()
      if args.before:
        before=json.loads(Path(args.before).read_text(encoding="utf-8"))
        unchanged=("artifactId","adoptedRevisionId","artifactStatus","memberId","memberStatus","pageRunId","pageRunStatus","assetId")
        if any(before.get(k)!=now.get(k) for k in unchanged): fail("UI_MANUAL_IDENTITY_CHANGED")
        if now["optimisticVersion"] != before.get("optimisticVersion", -1)+1: fail("UI_MANUAL_VERSION_INVALID")
        if now["candidateCount"] != before.get("candidateCount", -1)+1: fail("UI_MANUAL_CANDIDATE_COUNT_INVALID")
        created=[value for value in now["candidateIds"] if value not in before.get("candidateIds",[])]
        if len(created)!=1 or now["candidateHeadRevisionId"] != created[0] or now["candidateParents"].get(created[0]) != before["candidateHeadRevisionId"]: fail("UI_MANUAL_CANDIDATE_PARENT_INVALID")
      print(json.dumps({"ok":True,"snapshot":now},ensure_ascii=True))
    except (OSError,sqlite3.Error,RuntimeError,json.JSONDecodeError) as e: fail(str(e) or "UI_MANUAL_VERIFY_FAILED")
if __name__=="__main__": main()
