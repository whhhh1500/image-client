#!/usr/bin/env python3
"""Create a D-only, provider-disabled clone for reviewing one completed page.

This script never launches an application.  Its source connection is a
read-only SQLite snapshot and the only writes target the requested clone.
"""
from __future__ import annotations

import argparse, hashlib, json, os, shutil, sqlite3, stat, sys
from pathlib import Path


def die(code: str) -> None:
    raise SystemExit(code)


def d_local(path: Path, code: str, exists: bool = True) -> Path:
    raw = path.absolute()
    if raw.drive.upper() != "D:":
        die(code + "_NOT_D")
    for item in (raw, *raw.parents):
        if item.exists() and getattr(os.lstat(item), "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT:
            die(code + "_REPARSE")
    if exists:
        try:
            return raw.resolve(strict=True)
        except OSError:
            die(code + "_MISSING")
    return raw.resolve()


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as source:
        for part in iter(lambda: source.read(1024 * 1024), b""):
            h.update(part)
    return "sha256:" + h.hexdigest()


def clone_assets(source_root: Path, target_root: Path, clone: sqlite3.Connection) -> None:
    source_assets = d_local(source_root / "data" / "assets", "RECOVERY_SOURCE_ASSETS")
    target_assets = target_root / "data" / "assets"
    target_assets.mkdir(parents=True, exist_ok=False)
    rows = clone.execute("SELECT id,path FROM assets").fetchall()
    for asset_id, stored in rows:
        source = d_local(Path(stored), "RECOVERY_SOURCE_ASSET")
        try:
            relative = source.relative_to(source_assets)
        except ValueError:
            die("RECOVERY_SOURCE_ASSET_OUTSIDE_ROOT")
        destination = target_assets / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        if digest(source) != digest(destination):
            die("RECOVERY_ASSET_HASH_MISMATCH")
        if clone.execute("UPDATE assets SET path=? WHERE id=?", (str(destination), asset_id)).rowcount != 1:
            die("RECOVERY_ASSET_REWRITE_FAILED")


def main() -> None:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--source-runtime", required=True)
    parser.add_argument("--target-root", required=True)
    parser.add_argument("--normal-exe", required=True)
    parser.add_argument("--normal-sha256", required=True)
    args = parser.parse_args()
    source_root = d_local(Path(args.source_runtime), "RECOVERY_SOURCE")
    target_root = d_local(Path(args.target_root), "RECOVERY_TARGET", exists=False)
    if target_root.exists(): die("RECOVERY_TARGET_EXISTS")
    source_db = d_local(source_root / "data" / "image-client.db", "RECOVERY_SOURCE_DB")
    normal_exe = d_local(Path(args.normal_exe), "RECOVERY_NORMAL_EXE")
    if digest(normal_exe).lower() != ("sha256:" + args.normal_sha256.lower()): die("RECOVERY_NORMAL_HASH_MISMATCH")
    source_uri = source_db.as_uri() + "?mode=ro"
    ro = sqlite3.connect(source_uri, uri=True, isolation_level=None)
    ro.row_factory = sqlite3.Row
    try:
        ro.execute("PRAGMA query_only=ON"); ro.execute("BEGIN")
        bad = ro.execute("""SELECT COUNT(*) FROM novel_production_jobs WHERE status IN ('queued','running','blocked_config','needs_reconcile')
          UNION ALL SELECT COUNT(*) FROM comic_visual_batches WHERE status IN ('queued','running','blocked_config','needs_reconcile')
          UNION ALL SELECT COUNT(*) FROM comic_visual_page_runs WHERE status IN ('queued','running','blocked_config','needs_reconcile')""").fetchall()
        if any(row[0] for row in bad): die("RECOVERY_ACTIVE_WORK_REJECTED")
        rows = ro.execute("""SELECT b.project_id,b.novel_work_id,b.novel_chapter_id,b.source_revision_id,b.production_job_id,
          j.source_analysis_run_id,w.title work_title,c.title chapter_title,c.chapter_no,
          m.id member_id,m.page_run_id,r.asset_id,r.page_no
          FROM comic_visual_batches b JOIN novel_production_jobs j ON j.id=b.production_job_id
          JOIN novel_works w ON w.id=b.novel_work_id JOIN novel_chapters c ON c.id=b.novel_chapter_id
          JOIN comic_visual_batch_members m ON m.batch_id=b.id JOIN comic_visual_page_runs r ON r.id=m.page_run_id
          WHERE b.status='candidate_ready' AND m.status='candidate_ready' AND r.status='candidate_ready' AND r.asset_id IS NOT NULL""").fetchall()
        if len(rows) != 1 or rows[0]["page_no"] != 1: die("RECOVERY_CANDIDATE_NOT_UNIQUE")
        row = rows[0]
        target_db = target_root / "data" / "image-client.db"
        target_db.parent.mkdir(parents=True, exist_ok=False)
        target_root.mkdir(parents=True, exist_ok=True)
        clone = sqlite3.connect(target_db)
        try:
            ro.backup(clone)
            clone.execute("BEGIN IMMEDIATE")
            clone_assets(source_root, target_root, clone)
            project = {"id": row["project_id"], "name": "恢复验收项目", "description": "", "storyStyle": "通用短剧", "artStyle": "电影写实", "aspectRatio": "16:9", "imageModel": "", "imageQuality": "", "videoModel": ""}
            clone.execute("DELETE FROM settings")
            clone.execute("INSERT INTO settings(key,value) VALUES('projects',?)", (json.dumps([project], ensure_ascii=False, separators=(",", ":")),))
            clone.execute("INSERT INTO settings(key,value) VALUES('active_project',?)", (row["project_id"],))
            clone.commit()
        finally:
            clone.close()
    finally:
        ro.close()
    data_root = target_root / "data"
    (data_root / "backend-config.json").write_text(json.dumps({"image_api_url":"","image_api_key":"","llm_api_url":"","llm_api_key":"","video_api_url":"","video_api_key":"","output_dir":str(data_root / "assets")}, ensure_ascii=True), encoding="utf-8")
    scope = {"schemaVersion":"normal-ui-acceptance-scope.v1", "projectId":row["project_id"], "novelWorkId":row["novel_work_id"], "novelChapterId":row["novel_chapter_id"], "sourceRevisionId":row["source_revision_id"], "productionJobId":row["production_job_id"], "sourceAnalysisRunId":row["source_analysis_run_id"]}
    (target_root / "scope.json").write_text(json.dumps(scope, ensure_ascii=True), encoding="utf-8")
    context = {"schemaVersion":"normal-ui-recovery.v1", "runtimeRoot":str(target_root), "databasePath":str(target_db), "scopePath":str(target_root / "scope.json"), "normalExecutablePath":str(normal_exe), "normalSha256":args.normal_sha256.upper(), "workTitle":row["work_title"], "chapterTitle":row["chapter_title"], "chapterNo":row["chapter_no"], "memberId":row["member_id"], "pageRunId":row["page_run_id"], "assetId":row["asset_id"]}
    (target_root / "recovery-context.json").write_text(json.dumps(context, ensure_ascii=True), encoding="utf-8")
    check = sqlite3.connect(target_db)
    try:
        world = check.execute("""SELECT r.body_json FROM analysis_artifacts a
          JOIN analysis_artifact_revisions r ON r.id=a.adopted_head_revision_id
          WHERE a.source_analysis_run_id=? AND a.artifact_type='world_facts'""", (row["source_analysis_run_id"],)).fetchall()
    finally:
        check.close()
    if len(world) != 1: die("RECOVERY_WORLD_FACTS_NOT_UNIQUE")
    body = json.loads(world[0][0])
    content = body.get("content") if isinstance(body, dict) and isinstance(body.get("content"), dict) else body
    items = content.get("items") if isinstance(content, dict) else None
    if not isinstance(items, list): die("RECOVERY_WORLD_FACTS_ITEMS_INVALID")
    editable = next((index for index, item in enumerate(items) if isinstance(item, dict) and isinstance(item.get("statement"), str) and item["statement"].strip()), None)
    if editable is None: die("RECOVERY_WORLD_FACTS_STATEMENT_MISSING")
    prefix = ["content"] if content is not body else []
    label_prefix = "内容 · " if prefix else "items · "
    manual = {"schemaVersion":"normal-ui-recovery-manual-field.v1", "jsonPath":prefix + ["items",editable,"statement"], "ariaLabel":f"{label_prefix}第 {editable + 1} 项 · 陈述"}
    (target_root / "manual-field.json").write_text(json.dumps(manual, ensure_ascii=True), encoding="utf-8")
    print(json.dumps({"ok":True,"runtimeRoot":str(target_root),"scope":scope,"assetId":row["asset_id"]}, ensure_ascii=True))

if __name__ == "__main__": main()
