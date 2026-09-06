#!/usr/bin/env python3
"""Fail-closed read-only proof of one UI-initiated default candidate export."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sqlite3
import stat
from pathlib import Path


def fail(code: str) -> None:
    print(json.dumps({"ok": False, "code": code}, ensure_ascii=True))
    raise SystemExit(0)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return "sha256:" + digest.hexdigest()


def d_path_without_reparse(path: Path) -> Path:
    raw = path.absolute()
    for item in (raw, *raw.parents):
        if not item.exists():
            continue
        attributes = getattr(os.lstat(item), "st_file_attributes", 0)
        if attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT:
            raise ValueError("path has reparse ancestor")
    # Rust's Windows canonicalize persists local paths as `\\?\D:\...`.
    # Inspect that raw chain above, then compare the ordinary local spelling
    # so it has the same drive/path identity as the prepared clone root.
    raw_text = str(raw)
    if raw_text.startswith("\\\\?\\UNC\\"):
        raise ValueError("UNC paths are not a D-local export destination")
    normalized = Path(raw_text[4:]) if raw_text.startswith("\\\\?\\") else raw
    resolved = normalized.resolve(strict=True)
    if resolved.drive.upper() != "D:":
        raise ValueError("path is not on D")
    return resolved


def d_new_file_without_reparse(path: Path) -> Path:
    raw = path.absolute()
    if raw.exists():
        raise ValueError("snapshot already exists")
    parent = d_path_without_reparse(raw.parent)
    if parent.drive.upper() != "D:":
        raise ValueError("snapshot is not on D")
    return parent / raw.name


def child(root: Path, value: str) -> Path:
    raw = Path(value)
    resolved = d_path_without_reparse(raw)
    resolved.relative_to(root)
    return resolved


def same_lineage(value: dict, expected: dict) -> bool:
    return all(value.get(key) == expected[key] for key in expected)


def valid_component(value: object) -> bool:
    return isinstance(value, str) and bool(re.fullmatch(r"[A-Za-z0-9_-]{1,160}", value))


def validate_scope(scope: object) -> dict:
    keys = {"projectId", "novelWorkId", "novelChapterId", "sourceRevisionId", "productionJobId", "sourceAnalysisRunId"}
    if not isinstance(scope, dict) or not keys.issubset(scope) or any(not isinstance(scope[key], str) or not scope[key] for key in keys):
        fail("UI_EXPORT_SCOPE_INVALID")
    return scope


def read_candidate(conn: sqlite3.Connection, scope: dict) -> tuple[sqlite3.Row, sqlite3.Row, dict]:
    batches = conn.execute(
        """SELECT b.id FROM comic_visual_batches b
          JOIN novel_production_jobs j ON j.id=b.production_job_id
          WHERE b.production_job_id=:productionJobId AND b.project_id=:projectId AND b.novel_work_id=:novelWorkId
            AND b.novel_chapter_id=:novelChapterId AND b.source_revision_id=:sourceRevisionId
            AND j.project_id=:projectId AND j.novel_work_id=:novelWorkId AND j.novel_chapter_id=:novelChapterId
            AND j.source_revision_id=:sourceRevisionId AND j.source_analysis_run_id=:sourceAnalysisRunId""",
        scope,
    ).fetchall()
    if len(batches) != 1:
        fail("UI_EXPORT_BATCH_NOT_UNIQUE")
    batch = batches[0]
    visual = conn.execute(
        """SELECT m.id member_id,m.ordinal,m.manifest_id,m.production_chapter_id,m.production_page_id,m.status member_status,
            r.id run_id,r.status run_status,r.asset_id,a.path asset_path
            FROM comic_visual_batch_members m
            JOIN comic_visual_page_runs r ON r.id=m.page_run_id
            JOIN assets a ON a.id=r.asset_id
            WHERE m.batch_id=?""",
        (batch["id"],),
    ).fetchall()
    if len(visual) != 1:
        fail("UI_EXPORT_FILESET_NOT_UNIQUE")
    page = visual[0]
    if page["ordinal"] != 1 or page["member_status"] != "candidate_ready" or page["run_status"] != "candidate_ready":
        fail("UI_EXPORT_LINEAGE_INVALID")
    source = d_path_without_reparse(Path(page["asset_path"]))
    snapshot = {
        "batchId": batch["id"],
        "memberId": page["member_id"],
        "manifestId": page["manifest_id"],
        "productionChapterId": page["production_chapter_id"],
        "productionPageId": page["production_page_id"],
        "runId": page["run_id"],
        "assetId": page["asset_id"],
        "assetSha256": sha256(source),
    }
    return batch, page, snapshot


def expected_default_root(data_root: Path, scope: dict, page: sqlite3.Row) -> Path:
    if not valid_component(scope["novelWorkId"]) or not valid_component(page["production_chapter_id"]):
        fail("UI_EXPORT_DEFAULT_COMPONENT_INVALID")
    root = d_path_without_reparse(data_root)
    return child(root, str(root / "漫画导出" / scope["novelWorkId"] / page["production_chapter_id"]))


def load_before(path: Path) -> dict:
    value = json.loads(d_path_without_reparse(path).read_text(encoding="utf-8"))
    if not isinstance(value, dict) or value.get("schemaVersion") != "normal-ui-export-candidate.v1" or not isinstance(value.get("snapshot"), dict):
        fail("UI_EXPORT_BEFORE_INVALID")
    return value["snapshot"]


def main() -> None:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--database", required=True)
    parser.add_argument("--scope", required=True)
    parser.add_argument("--data-root", required=True)
    parser.add_argument("--snapshot-out")
    parser.add_argument("--before-candidate")
    args = parser.parse_args()
    if bool(args.snapshot_out) == bool(args.before_candidate):
        fail("UI_EXPORT_MODE_INVALID")
    try:
        data_root = d_path_without_reparse(Path(args.data_root))
        scope = validate_scope(json.loads(d_path_without_reparse(Path(args.scope)).read_text(encoding="utf-8")))
        database = d_path_without_reparse(Path(args.database))
        conn = sqlite3.connect(database.as_uri() + "?mode=ro", uri=True, isolation_level=None)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA query_only=ON")
        conn.execute("BEGIN")
        batch, page, snapshot = read_candidate(conn, scope)
        if args.snapshot_out:
            output = d_new_file_without_reparse(Path(args.snapshot_out))
            output.write_text(json.dumps({"schemaVersion": "normal-ui-export-candidate.v1", "snapshot": snapshot}, ensure_ascii=True), encoding="utf-8")
            conn.close()
            print(json.dumps({"ok": True, "mode": "snapshot"}, ensure_ascii=True))
            return

        if snapshot != load_before(Path(args.before_candidate)):
            fail("UI_EXPORT_CANDIDATE_CHANGED")
        default_root = expected_default_root(data_root, scope, page)
        exports = conn.execute(
            """SELECT id,destination_dir,directory_path,manifest_path,status,selection_json
              FROM comic_visual_exports WHERE batch_id=? AND project_id=? AND novel_work_id=?""",
            (batch["id"], scope["projectId"], scope["novelWorkId"]),
        ).fetchall()
        matched = []
        for export in exports:
            try:
                if d_path_without_reparse(Path(export["destination_dir"])) == default_root:
                    matched.append(export)
            except (OSError, ValueError):
                continue
        if len(matched) != 1 or matched[0]["status"] != "complete":
            fail("UI_EXPORT_NOT_COMPLETE")
        export = matched[0]
        receipt = conn.execute(
            "SELECT export_id FROM comic_visual_export_command_receipts WHERE command_name='comic_visual_batch_export' AND export_id=?",
            (export["id"],),
        ).fetchall()
        files = conn.execute(
            "SELECT member_id,ordinal,manifest_id,production_chapter_id,production_page_id,run_id,asset_id,path,sha256 FROM comic_visual_export_files WHERE export_id=?",
            (export["id"],),
        ).fetchall()
        conn.close()
        if len(receipt) != 1 or len(files) != 1:
            fail("UI_EXPORT_FILESET_NOT_UNIQUE")
        file = files[0]
        expected_db = {
            "member_id": page["member_id"], "ordinal": 1, "manifest_id": page["manifest_id"],
            "production_chapter_id": page["production_chapter_id"], "production_page_id": page["production_page_id"],
            "run_id": page["run_id"], "asset_id": page["asset_id"],
        }
        expected_manifest = {
            "memberId": page["member_id"], "ordinal": 1, "manifestId": page["manifest_id"],
            "productionChapterId": page["production_chapter_id"], "productionPageId": page["production_page_id"],
            "runId": page["run_id"], "assetId": page["asset_id"],
        }
        if not same_lineage(dict(file), expected_db):
            fail("UI_EXPORT_LINEAGE_INVALID")
        directory = child(default_root, export["directory_path"])
        manifest = child(default_root, export["manifest_path"])
        exported = child(default_root, file["path"])
        if directory.parent != default_root or not directory.name.startswith("comic-pages-") or manifest.parent != directory or exported.parent != directory or manifest.name != "manifest.json" or exported.name != "001-page-1.png":
            fail("UI_EXPORT_PATH_INVALID")
        if {item.name for item in directory.iterdir()} != {"manifest.json", "001-page-1.png"}:
            fail("UI_EXPORT_DIRECTORY_CONTENTS_INVALID")
        selection = json.loads(manifest.read_text(encoding="utf-8"))
        stored_selection = json.loads(export["selection_json"])
        manifest_files = selection.get("files") if isinstance(selection, dict) else None
        if selection.get("kind") != "full_batch" or selection.get("candidate") is not True or not isinstance(manifest_files, list) or len(manifest_files) != 1 or not same_lineage(manifest_files[0], expected_manifest) or manifest_files[0].get("sha256") != file["sha256"]:
            fail("UI_EXPORT_MANIFEST_INVALID")
        if stored_selection != selection:
            fail("UI_EXPORT_SELECTION_MISMATCH")
        exported_hash = sha256(exported)
        if exported_hash != file["sha256"] or exported_hash != snapshot["assetSha256"]:
            fail("UI_EXPORT_HASH_INVALID")
        print(json.dumps({"ok": True, "exportId": export["id"], "directory": str(directory), "file": exported.name, "sha256": exported_hash}, ensure_ascii=True))
    except (OSError, sqlite3.Error, ValueError, json.JSONDecodeError):
        fail("UI_EXPORT_VERIFY_FAILED")


if __name__ == "__main__":
    main()
