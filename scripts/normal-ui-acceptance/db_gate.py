#!/usr/bin/env python3
"""Read-only SQLite gates for the normal-UI acceptance proxy.

This process accepts only a small, already-sanitised scope request.  It never
receives model prompts, provider responses, or credentials.  Every read opens
the database with SQLite's ``mode=ro`` URI and an explicit read transaction.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
from pathlib import Path
from urllib.parse import quote


class GateError(Exception):
    pass


def fail(code: str) -> None:
    print(json.dumps({"ok": False, "code": code}, ensure_ascii=True))
    raise SystemExit(0)


def open_read_only(path_text: str) -> sqlite3.Connection:
    path = Path(path_text)
    if not path.is_file():
        raise GateError("UI_PROXY_DB_MISSING")
    # ``as_uri`` percent-encodes the local path.  mode=ro prevents SQLite from
    # creating a journal, WAL, or schema side effect in the application's D
    # runtime root; query_only remains an explicit second defence.
    uri = path.resolve().as_uri() + "?mode=ro"
    conn = sqlite3.connect(uri, uri=True, isolation_level=None)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA query_only=ON")
    conn.execute("BEGIN")
    return conn


def exactly_one(rows: list[sqlite3.Row], code: str) -> sqlite3.Row:
    if not rows:
        raise GateError(f"{code}_NONE")
    if len(rows) != 1:
        raise GateError(f"{code}_MULTIPLE")
    return rows[0]


def source_gate(conn: sqlite3.Connection, scope: dict) -> dict:
    rows = conn.execute(
        """
        SELECT job.id AS job_id, job.attempt_no AS job_attempt_no,
               source.id AS source_run_id, attempt.attempt_no AS source_attempt_no,
               job.default_adaptation_id AS comic_adaptation_id,
               attempt.id AS attempt_id
        FROM novel_production_jobs AS job
        JOIN source_analysis_runs AS source ON source.novel_chapter_revision_id=job.source_revision_id
        JOIN novel_analysis_lineages AS lineage ON lineage.id=source.novel_analysis_lineage_id
        JOIN source_analysis_run_attempts AS attempt ON attempt.source_analysis_run_id=source.id
        WHERE job.id=:productionJobId AND job.project_id=:projectId AND job.novel_work_id=:novelWorkId
          AND job.novel_chapter_id=:novelChapterId
          AND job.source_revision_id=:sourceRevisionId
          AND job.status='running' AND job.stage='source_analysis'
          AND source.status='running'
          AND lineage.novel_work_id=job.novel_work_id
          AND source.frozen_comic_adaptation_id=job.default_adaptation_id
          AND source.idempotency_key=job.id || ':source:' || job.attempt_no
          AND source.created_at>=job.created_at
          AND attempt.status='running' AND attempt.lease_owner IS NOT NULL
          AND attempt.lease_expires_at>=CAST(unixepoch('now') * 1000 AS INTEGER)
          AND (job.source_analysis_run_id IS NULL OR job.source_analysis_run_id=source.id)
        """,
        scope,
    ).fetchall()
    row = exactly_one(rows, "UI_PROXY_SOURCE_SCOPE_NOT_UNIQUE")
    if row["source_run_id"] != scope["sourceAnalysisRunId"]:
        raise GateError("UI_PROXY_SOURCE_SCOPE_MISMATCH")
    if not row["comic_adaptation_id"]:
        raise GateError("UI_PROXY_SOURCE_ADAPTATION_BINDING_MISSING")
    return {
        "jobId": row["job_id"],
        "jobAttemptNo": row["job_attempt_no"],
        "sourceAnalysisRunId": row["source_run_id"],
        "sourceAttemptNo": row["source_attempt_no"],
        "comicAdaptationId": row["comic_adaptation_id"],
    }


def adaptation_gate(conn: sqlite3.Connection, scope: dict) -> dict:
    required = (
        "adaptationAnalysisRunId",
        "comicAdaptationId",
        "comicAdaptationChapterId",
    )
    if any(not isinstance(scope.get(field), str) or not scope[field] for field in required):
        raise GateError("UI_PROXY_ADAPTATION_SCOPE_INVALID")
    rows = conn.execute(
        """
        SELECT job.id AS job_id, job.attempt_no AS job_attempt_no,
               source.id AS source_run_id,
               adaptation.id AS adaptation_run_id,
               adaptation_attempt.attempt_no AS adaptation_attempt_no,
               adaptation_attempt.id AS adaptation_attempt_id
        FROM novel_production_jobs AS job
        JOIN source_analysis_runs AS source ON source.id=job.source_analysis_run_id
        JOIN novel_analysis_lineages AS lineage ON lineage.id=source.novel_analysis_lineage_id
        JOIN adaptation_analysis_runs AS adaptation
          ON adaptation.project_id=job.project_id
         AND adaptation.novel_work_id=job.novel_work_id
         AND adaptation.comic_adaptation_id=job.default_adaptation_id
        JOIN comic_adaptation_chapters AS adaptation_chapter
          ON adaptation_chapter.id=adaptation.comic_adaptation_chapter_id
        JOIN adaptation_analysis_run_attempts AS adaptation_attempt
          ON adaptation_attempt.adaptation_analysis_run_id=adaptation.id
        WHERE job.id=:productionJobId AND job.project_id=:projectId
          AND job.novel_work_id=:novelWorkId AND job.novel_chapter_id=:novelChapterId
          AND job.source_revision_id=:sourceRevisionId
          -- The formal lifecycle holds ensuring_adaptation throughout the
          -- provider call; adaptation_analysis is only written afterward.
          AND job.status='running' AND job.stage='ensuring_adaptation'
          AND source.id=:sourceAnalysisRunId AND source.status='ready_for_review'
          AND source.novel_chapter_revision_id=job.source_revision_id
          AND lineage.novel_work_id=job.novel_work_id
          AND source.frozen_comic_adaptation_id=job.default_adaptation_id
          AND adaptation.status='running'
          AND adaptation.comic_adaptation_id=:comicAdaptationId
          AND adaptation.comic_adaptation_chapter_id=:comicAdaptationChapterId
          AND adaptation_chapter.novel_chapter_revision_id=job.source_revision_id
          AND adaptation.idempotency_key=job.id || ':adaptation:' || job.attempt_no
          AND adaptation.created_at>=job.created_at
          AND adaptation.attempt_no=adaptation_attempt.attempt_no
          AND adaptation_attempt.status='running'
          AND adaptation.lease_owner IS NOT NULL AND TRIM(adaptation.lease_owner)<>''
          AND adaptation_attempt.lease_owner=adaptation.lease_owner
          AND adaptation_attempt.lease_owner IS NOT NULL AND TRIM(adaptation_attempt.lease_owner)<>''
          AND adaptation.lease_expires_at>=CAST(unixepoch('now') * 1000 AS INTEGER)
          AND adaptation_attempt.lease_expires_at=adaptation.lease_expires_at
          AND adaptation_attempt.lease_expires_at>=CAST(unixepoch('now') * 1000 AS INTEGER)
          AND (job.adaptation_analysis_run_id IS NULL OR job.adaptation_analysis_run_id=adaptation.id)
          AND (
            (adaptation.input_mode='source_run' AND adaptation.source_analysis_run_id=source.id)
            OR (
              adaptation.input_mode='artifact_revisions'
              AND (SELECT COUNT(*) FROM adaptation_analysis_run_inputs AS input
                   WHERE input.adaptation_analysis_run_id=adaptation.id)=10
              AND NOT EXISTS (
                SELECT 1
                FROM adaptation_analysis_run_inputs AS input
                LEFT JOIN analysis_artifact_revisions AS revision
                  ON revision.id=input.analysis_artifact_revision_id
                LEFT JOIN analysis_artifacts AS artifact
                  ON artifact.id=revision.analysis_artifact_id
                WHERE input.adaptation_analysis_run_id=adaptation.id
                  AND (
                    NOT (
                      (input.source_order=0 AND input.artifact_type='chapter_summary')
                      OR (input.source_order=1 AND input.artifact_type='chapter_beats')
                      OR (input.source_order=2 AND input.artifact_type='world_facts')
                      OR (input.source_order=3 AND input.artifact_type='character_facts')
                      OR (input.source_order=4 AND input.artifact_type='faction_facts')
                      OR (input.source_order=5 AND input.artifact_type='location_facts')
                      OR (input.source_order=6 AND input.artifact_type='prop_facts')
                      OR (input.source_order=7 AND input.artifact_type='timeline_delta')
                      OR (input.source_order=8 AND input.artifact_type='continuity_delta')
                      OR (input.source_order=9 AND input.artifact_type='open_threads')
                    )
                    OR artifact.source_analysis_run_id IS NOT source.id
                    OR artifact.adopted_head_revision_id IS NOT input.analysis_artifact_revision_id
                    OR revision.status IS NOT 'adopted'
                  )
              )
            )
          )
        """,
        scope,
    ).fetchall()
    row = exactly_one(rows, "UI_PROXY_ADAPTATION_SCOPE_NOT_UNIQUE")
    if row["adaptation_run_id"] != scope["adaptationAnalysisRunId"]:
        raise GateError("UI_PROXY_ADAPTATION_SCOPE_MISMATCH")
    return {
        "jobId": row["job_id"],
        "jobAttemptNo": row["job_attempt_no"],
        "sourceAnalysisRunId": row["source_run_id"],
        "adaptationAnalysisRunId": row["adaptation_run_id"],
        "adaptationAttemptNo": row["adaptation_attempt_no"],
    }


def as_object(raw: str, code: str) -> dict:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise GateError(code) from error
    if not isinstance(value, dict):
        raise GateError(code)
    return value


def finite_number(value: object) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def outer_edge_is_slanted(points: list[dict], edge: str) -> bool:
    """Match the frozen hero template's non-rectangular top divider.

    The exact production validator treats the visible outer edge of each top
    panel as slanted when its top and bottom x coordinates differ.  A plain
    rectangle must not pass the acceptance gate merely because its bounding
    box looks like the template.
    """
    min_y = min(float(point["y"]) for point in points)
    max_y = max(float(point["y"]) for point in points)
    top = [float(point["x"]) for point in points if abs(float(point["y"]) - min_y) < 1e-6]
    bottom = [float(point["x"]) for point in points if abs(float(point["y"]) - max_y) < 1e-6]
    if not top or not bottom:
        return False
    top_x = max(top) if edge == "right" else min(top)
    bottom_x = max(bottom) if edge == "right" else min(bottom)
    return abs(top_x - bottom_x) >= 0.02


def approved_hero_middle_geometry(layout: object) -> bool:
    if not isinstance(layout, dict):
        return False
    if (layout.get("templateId") != "hero_middle_5"
            or layout.get("layoutKind") != "template"
            or layout.get("readingOrder") != [1, 2, 3, 4, 5]
            or layout.get("dominantPanel") != 3):
        return False
    geometry = layout.get("geometry")
    if not isinstance(geometry, dict) or geometry.get("panelCount") != 5:
        return False
    safe_area = geometry.get("safeArea")
    if not isinstance(safe_area, dict) or not all(
        finite_number(safe_area.get(key)) for key in ("x", "y", "width", "height")
    ):
        return False
    entries = geometry.get("panels")
    if not isinstance(entries, list) or len(entries) != 5:
        return False
    by_no: dict[int, tuple[float, float, float, float]] = {}
    polygons: dict[int, list[dict]] = {}
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get("panelNo"), int):
            return False
        bounds = entry.get("bounds")
        polygon = entry.get("polygon")
        text_zone = entry.get("textZone")
        if (not isinstance(bounds, dict)
                or not all(finite_number(bounds.get(key)) for key in ("x", "y", "width", "height"))
                or not isinstance(polygon, list) or len(polygon) < 3
                or not all(isinstance(point, dict) and finite_number(point.get("x")) and finite_number(point.get("y")) for point in polygon)
                or not isinstance(text_zone, dict)):
            return False
        by_no[entry["panelNo"]] = (
            float(bounds["x"]), float(bounds["y"]), float(bounds["width"]), float(bounds["height"])
        )
        polygons[entry["panelNo"]] = polygon
    if set(by_no) != {1, 2, 3, 4, 5}:
        return False
    top_left, top_right, middle, bottom_left, bottom_right = (by_no[index] for index in range(1, 6))
    return (
        top_left[1] <= 0.06 and top_right[1] <= 0.06 and top_left[0] < top_right[0]
        and abs(top_left[2] - top_right[2]) >= 0.02
        and outer_edge_is_slanted(polygons[1], "right")
        and outer_edge_is_slanted(polygons[2], "left")
        and middle[0] <= 0.06 and middle[2] >= 0.9 and 0.24 <= middle[1] <= 0.30 and middle[3] >= 0.35
        and bottom_left[1] >= 0.66 and bottom_right[1] >= 0.66 and bottom_left[0] < bottom_right[0]
        and abs(bottom_left[2] - bottom_right[2]) >= 0.02
    )


def review_text_snapshot(page: dict) -> list[dict]:
    entries = []
    for panel in page["panels"]:
        spec = panel["spec"]
        entries.append({
            "panelNo": panel.get("panelNo"),
            "characterKeys": spec.get("characterKeys", []),
            "action": spec.get("action"),
            "visualBeat": spec.get("visualBeat"),
            "shot": spec.get("shot"),
            "narrativeFunction": spec.get("narrativeFunction"),
            "dialogues": spec.get("dialogues", []),
            "captions": spec.get("captions", []),
            "soundEffects": spec.get("soundEffects", []),
        })
    return entries


def image_gate(conn: sqlite3.Connection, scope: dict) -> dict:
    rows = conn.execute(
        """
        SELECT batch.id AS batch_id, member.id AS member_id, manifest.id AS manifest_id,
               manifest.manifest_fingerprint AS manifest_fingerprint,
               manifest.manifest_json AS manifest_json, page_run.id AS page_run_id,
               page_run.compiler_contract_version AS compiler_contract_version,
               page_run.status AS page_run_status, page_run.production_page_id AS page_id,
               job.comic_plan_intent_json AS comic_plan_intent_json
        FROM novel_production_jobs AS job
        JOIN source_analysis_runs AS source ON source.id=job.source_analysis_run_id
        JOIN adaptation_analysis_runs AS adaptation ON adaptation.id=job.adaptation_analysis_run_id
        JOIN analysis_apply_operations AS apply_op ON apply_op.id=job.apply_operation_id
        JOIN comic_visual_batches AS batch ON batch.production_job_id=job.id
        JOIN comic_visual_batch_members AS member ON member.batch_id=batch.id
        JOIN comic_visual_manifests AS manifest ON manifest.id=member.manifest_id
        JOIN comic_visual_page_runs AS page_run ON page_run.id=member.page_run_id
        JOIN comic_production_pages AS production_page ON production_page.id=member.production_page_id
        WHERE job.id=:productionJobId AND job.project_id=:projectId
          AND job.novel_work_id=:novelWorkId AND job.novel_chapter_id=:novelChapterId
          AND job.source_revision_id=:sourceRevisionId
          AND source.id=:sourceAnalysisRunId
          AND job.status='succeeded' AND job.stage='succeeded'
          AND source.status='ready_for_review' AND adaptation.status='ready_for_review'
          AND apply_op.operation_type='apply_comic_plan' AND apply_op.status='succeeded'
          AND batch.project_id=job.project_id AND batch.novel_work_id=job.novel_work_id
          AND batch.comic_adaptation_id=job.default_adaptation_id
          AND batch.apply_operation_id=job.apply_operation_id
          AND batch.status IN ('preparing','running') AND batch.total_members=1
          AND manifest.apply_operation_id=job.apply_operation_id
          AND manifest.project_id=job.project_id AND manifest.novel_work_id=job.novel_work_id
          AND manifest.comic_adaptation_id=job.default_adaptation_id
          AND manifest.contract_version='comic-visual.v1'
          AND manifest.production_chapter_id=member.production_chapter_id
          AND production_page.comic_production_chapter_id=manifest.production_chapter_id
          AND member.ordinal=1 AND member.status IN ('queued','running')
          AND page_run.status IN ('queued','running')
          AND page_run.compiler_contract_version='comic-page-compiler.v3'
          AND page_run.manifest_id=manifest.id AND page_run.production_page_id=member.production_page_id
        """,
        scope,
    ).fetchall()
    row = exactly_one(rows, "UI_PROXY_IMAGE_SCOPE_NOT_UNIQUE")
    manifest = as_object(row["manifest_json"], "UI_PROXY_IMAGE_MANIFEST_INVALID")
    intent = as_object(row["comic_plan_intent_json"], "UI_PROXY_IMAGE_INTENT_INVALID")
    pages = manifest.get("pages")
    intent_pages = intent.get("pages")
    if not isinstance(pages, list) or len(pages) != 1:
        raise GateError("UI_PROXY_IMAGE_MANIFEST_NOT_ONE_PAGE")
    if not isinstance(intent_pages, list) or len(intent_pages) != 1:
        raise GateError("UI_PROXY_IMAGE_INTENT_NOT_ONE_PAGE")
    page = pages[0]
    intent_page = intent_pages[0]
    if not isinstance(page, dict) or not isinstance(intent_page, dict):
        raise GateError("UI_PROXY_IMAGE_MANIFEST_INVALID")
    panels = page.get("panels")
    geometry = page.get("layout")
    if not isinstance(panels, list) or len(panels) != 5:
        raise GateError("UI_PROXY_IMAGE_PANEL_COUNT_INVALID")
    if (intent_page.get("panelCount") != 5
            or intent_page.get("layoutProfile") != "hero_middle_5"):
        raise GateError("UI_PROXY_IMAGE_INTENT_PANEL_COUNT_INVALID")
    if not approved_hero_middle_geometry(geometry):
        raise GateError("UI_PROXY_IMAGE_GEOMETRY_INVALID")
    if page.get("productionPageId") != row["page_id"]:
        raise GateError("UI_PROXY_IMAGE_PAGE_BINDING_INVALID")
    sound_effect_count = 0
    dialogue_chars = 0
    for panel in panels:
        if not isinstance(panel, dict):
            raise GateError("UI_PROXY_IMAGE_MANIFEST_INVALID")
        spec = panel.get("spec")
        if not isinstance(spec, dict):
            raise GateError("UI_PROXY_IMAGE_MANIFEST_INVALID")
        effects = spec.get("soundEffects")
        if isinstance(effects, list):
            sound_effect_count += sum(1 for value in effects if isinstance(value, str) and value)
        dialogues = spec.get("dialogues")
        if isinstance(dialogues, list):
            for value in dialogues:
                if isinstance(value, str):
                    dialogue_chars += len(value)
                elif isinstance(value, dict) and isinstance(value.get("text"), str):
                    dialogue_chars += len(value["text"])
    if sound_effect_count < 1:
        raise GateError("UI_PROXY_IMAGE_SFX_REQUIRED")
    # A tiny bound is intentional: this is a visual QA sample, not a loophole
    # for sending an unbounded chapter transcript through the image endpoint.
    if dialogue_chars <= 0 or dialogue_chars > 160:
        raise GateError("UI_PROXY_IMAGE_DIALOGUE_BOUND_INVALID")
    return {
        "jobId": scope["productionJobId"],
        "batchId": row["batch_id"],
        "memberId": row["member_id"],
        "manifestId": row["manifest_id"],
        "manifestFingerprint": row["manifest_fingerprint"],
        "pageRunId": row["page_run_id"],
        "soundEffectCount": sound_effect_count,
        "dialogueChars": dialogue_chars,
        "reviewSnapshot": {
            "pageNo": page.get("pageNo"),
            "productionPageId": page.get("productionPageId"),
            "layout": geometry,
            "textByPanel": review_text_snapshot(page),
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--database", required=True)
    parser.add_argument("--kind", choices=("source", "adaptation", "image"), required=True)
    parser.add_argument("--scope", required=True)
    args = parser.parse_args()
    try:
        scope = json.loads(Path(args.scope).read_text(encoding="utf-8"))
        if not isinstance(scope, dict):
            raise GateError("UI_PROXY_SCOPE_INVALID")
        conn = open_read_only(args.database)
        try:
            if args.kind == "source":
                result = source_gate(conn, scope)
            elif args.kind == "adaptation":
                result = adaptation_gate(conn, scope)
            else:
                result = image_gate(conn, scope)
            print(json.dumps({"ok": True, "result": result}, ensure_ascii=True))
        finally:
            conn.close()
    except (OSError, sqlite3.Error, GateError, json.JSONDecodeError) as error:
        fail(error.args[0] if error.args and isinstance(error.args[0], str) else "UI_PROXY_DB_GATE_FAILED")


if __name__ == "__main__":
    main()
