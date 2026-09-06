#!/usr/bin/env python3
"""D-only fake-schema checks for the two read-only normal-UI helpers."""
from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(r"D:\cc\image-client")
SCRIPT_ROOT = ROOT / "scripts" / "normal-ui-acceptance"
TEST_PARENT = ROOT / ".test-tmp" / "normal-ui-helper-tests"


def run_json(*args: str) -> dict:
    result = subprocess.run([sys.executable, *args], text=True, encoding="utf-8", capture_output=True, check=False)
    if result.returncode != 0:
        raise AssertionError(f"helper exited {result.returncode}: {result.stderr}")
    return json.loads(result.stdout)


def run_source_gate(database: Path, scope: dict, scope_path: Path) -> dict:
    scope_path.write_text(json.dumps(scope), encoding="utf-8")
    return run_json(
        str(SCRIPT_ROOT / "db_gate.py"), "--database", str(database),
        "--kind", "source", "--scope", str(scope_path),
    )


def run_adaptation_gate(database: Path, scope: dict, scope_path: Path) -> dict:
    scope_path.write_text(json.dumps(scope), encoding="utf-8")
    return run_json(
        str(SCRIPT_ROOT / "db_gate.py"), "--database", str(database),
        "--kind", "adaptation", "--scope", str(scope_path),
    )


PRODUCTION_SOURCE_TYPES = (
    "chapter_summary", "chapter_beats", "world_facts", "character_facts",
    "faction_facts", "location_facts", "prop_facts", "timeline_delta",
    "continuity_delta", "open_threads",
)


def make_scope_database(path: Path, content_hash: str) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE novel_works(id TEXT PRIMARY KEY, project_id TEXT, title TEXT);
        CREATE TABLE novel_chapters(id TEXT PRIMARY KEY, novel_work_id TEXT, title TEXT, current_revision_id TEXT);
        CREATE TABLE novel_chapter_revisions(id TEXT PRIMARY KEY, content_hash TEXT);
        CREATE TABLE novel_analysis_lineages(id TEXT PRIMARY KEY, novel_work_id TEXT);
        CREATE TABLE novel_production_jobs(id TEXT PRIMARY KEY, project_id TEXT, novel_work_id TEXT, novel_chapter_id TEXT, source_revision_id TEXT, default_adaptation_id TEXT, source_analysis_run_id TEXT, adaptation_analysis_run_id TEXT, status TEXT, stage TEXT, attempt_no INTEGER, created_at INTEGER);
        CREATE TABLE source_analysis_runs(id TEXT PRIMARY KEY, novel_chapter_revision_id TEXT, novel_analysis_lineage_id TEXT, frozen_comic_adaptation_id TEXT, idempotency_key TEXT, status TEXT, created_at INTEGER);
        CREATE TABLE source_analysis_run_attempts(id TEXT PRIMARY KEY, source_analysis_run_id TEXT, attempt_no INTEGER, status TEXT, lease_owner TEXT, lease_expires_at INTEGER);
        CREATE TABLE comic_adaptation_chapters(id TEXT PRIMARY KEY, comic_adaptation_id TEXT, novel_chapter_revision_id TEXT);
        CREATE TABLE adaptation_analysis_runs(id TEXT PRIMARY KEY, project_id TEXT, novel_work_id TEXT, comic_adaptation_id TEXT, comic_adaptation_chapter_id TEXT, input_mode TEXT, source_analysis_run_id TEXT, idempotency_key TEXT, status TEXT, attempt_no INTEGER, lease_owner TEXT, lease_expires_at INTEGER, created_at INTEGER);
        CREATE TABLE adaptation_analysis_run_attempts(id TEXT PRIMARY KEY, adaptation_analysis_run_id TEXT, attempt_no INTEGER, status TEXT, lease_owner TEXT, lease_expires_at INTEGER);
        CREATE TABLE analysis_artifacts(id TEXT PRIMARY KEY, source_analysis_run_id TEXT, artifact_type TEXT, adopted_head_revision_id TEXT);
        CREATE TABLE analysis_artifact_revisions(id TEXT PRIMARY KEY, analysis_artifact_id TEXT, status TEXT);
        CREATE TABLE adaptation_analysis_run_inputs(adaptation_analysis_run_id TEXT, artifact_type TEXT, analysis_artifact_revision_id TEXT, source_order INTEGER);
        INSERT INTO novel_works VALUES('project-work','project-1','验收小说-单元');
        INSERT INTO novel_chapter_revisions VALUES('revision-1','%s');
        INSERT INTO novel_chapters VALUES('chapter-1','project-work','验收章节-单元','revision-1');
        INSERT INTO novel_analysis_lineages VALUES('lineage-1','project-work');
        INSERT INTO source_analysis_runs VALUES('source-run-1','revision-1','lineage-1','adaptation-1','job-1:source:1','running',1000);
        INSERT INTO source_analysis_run_attempts VALUES('attempt-1','source-run-1',1,'running','owner-1',4102444800000);
        INSERT INTO novel_production_jobs VALUES('job-1','project-1','project-work','chapter-1','revision-1','adaptation-1',NULL,NULL,'running','source_analysis',1,1);
        """ % content_hash
    )
    conn.commit(); conn.close()


def activate_adaptation_prelink(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        UPDATE source_analysis_runs SET status='ready_for_review' WHERE id='source-run-1';
        UPDATE novel_production_jobs SET source_analysis_run_id='source-run-1', status='running', stage='ensuring_adaptation', adaptation_analysis_run_id=NULL WHERE id='job-1';
        INSERT INTO comic_adaptation_chapters VALUES('adaptation-chapter-1','adaptation-1','revision-1');
        INSERT INTO adaptation_analysis_runs VALUES('adaptation-run-1','project-1','project-work','adaptation-1','adaptation-chapter-1','artifact_revisions',NULL,'job-1:adaptation:1','running',1,'adapt-owner',4102444800000,1000);
        INSERT INTO adaptation_analysis_run_attempts VALUES('adapt-attempt-1','adaptation-run-1',1,'running','adapt-owner',4102444800000);
        """
    )
    for order, artifact_type in enumerate(PRODUCTION_SOURCE_TYPES):
        artifact_id = f"source-artifact-{order}"
        revision_id = f"source-revision-{order}"
        conn.execute("INSERT INTO analysis_artifacts VALUES(?,?,?,?)", (artifact_id, "source-run-1", artifact_type, revision_id))
        conn.execute("INSERT INTO analysis_artifact_revisions VALUES(?,?,?)", (revision_id, artifact_id, "adopted"))
        conn.execute("INSERT INTO adaptation_analysis_run_inputs VALUES(?,?,?,?)", ("adaptation-run-1", artifact_type, revision_id, order))
    conn.commit(); conn.close()


def make_manual_database(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE analysis_artifacts(id TEXT PRIMARY KEY, source_analysis_run_id TEXT, novel_work_id TEXT, novel_chapter_revision_id TEXT, artifact_type TEXT, adopted_head_revision_id TEXT, candidate_head_revision_id TEXT, status TEXT, optimistic_version INTEGER);
        CREATE TABLE analysis_artifact_revisions(id TEXT PRIMARY KEY, analysis_artifact_id TEXT, parent_revision_id TEXT, status TEXT);
        CREATE TABLE comic_visual_batches(id TEXT PRIMARY KEY, production_job_id TEXT, project_id TEXT, novel_work_id TEXT, novel_chapter_id TEXT, source_revision_id TEXT);
        CREATE TABLE comic_visual_batch_members(id TEXT PRIMARY KEY, batch_id TEXT, ordinal INTEGER, manifest_id TEXT, production_chapter_id TEXT, production_page_id TEXT, status TEXT, page_run_id TEXT);
        CREATE TABLE comic_visual_page_runs(id TEXT PRIMARY KEY, status TEXT, asset_id TEXT);
        CREATE TABLE assets(id TEXT PRIMARY KEY, path TEXT);
        INSERT INTO analysis_artifacts VALUES('artifact-world','source-run-1','work-1','revision-1','world_facts','revision-adopted','revision-candidate-old','active',7);
        INSERT INTO analysis_artifact_revisions VALUES('revision-candidate-old','artifact-world','revision-prior','candidate');
        INSERT INTO comic_visual_batches VALUES('batch-1','job-1','project-1','work-1','chapter-1','revision-1');
        INSERT INTO comic_visual_page_runs VALUES('page-run-1','candidate_ready','asset-1');
        INSERT INTO comic_visual_batch_members VALUES('member-1','batch-1',1,'manifest-1','production-chapter-1','production-page-1','candidate_ready','page-run-1');
        """
    )
    conn.commit(); conn.close()


class ReadOnlyHelperTests(unittest.TestCase):
    def setUp(self) -> None:
        TEST_PARENT.mkdir(parents=True, exist_ok=True)
        self.temp = Path(tempfile.mkdtemp(prefix="helpers-", dir=TEST_PARENT))

    def test_scope_uses_utf8_sha256_and_never_returns_body(self) -> None:
        body = "成年人雨巷：啪嗒，哐当。"
        content_hash = "sha256:" + hashlib.sha256(body.encode("utf-8")).hexdigest()
        database = self.temp / "scope.db"; make_scope_database(database, content_hash)
        args = [str(SCRIPT_ROOT / "register_ui_scope.py"), "--database", str(database), "--novel", "验收小说-单元", "--chapter", "验收章节-单元", "--content-hash", content_hash, "--wait-ms", "1"]
        output = run_json(*args)
        self.assertTrue(output["ok"])
        self.assertEqual(output["scope"]["revision_id"], "revision-1")
        self.assertNotIn(body, json.dumps(output, ensure_ascii=False))
        bad = run_json(
            str(SCRIPT_ROOT / "register_ui_scope.py"), "--database", str(database),
            "--novel", "验收小说-单元", "--chapter", "验收章节-单元",
            "--content-hash", "sha256:" + "0" * 64, "--wait-ms", "1",
        )
        self.assertFalse(bad["ok"]); self.assertEqual(bad["code"], "UI_SCOPE_NOT_READY")

    def test_source_prelink_registration_and_gate_bind_one_live_attempt(self) -> None:
        content_hash = "sha256:" + "1" * 64
        database = self.temp / "source-prelink.db"; make_scope_database(database, content_hash)
        registered = run_json(
            str(SCRIPT_ROOT / "register_ui_scope.py"), "--database", str(database),
            "--novel", "验收小说-单元", "--chapter", "验收章节-单元",
            "--content-hash", content_hash, "--wait-ms", "1",
        )
        self.assertTrue(registered["ok"])
        self.assertEqual(registered["scope"]["job_id"], "job-1")
        self.assertEqual(registered["scope"]["source_run_id"], "source-run-1")
        self.assertEqual(registered["scope"]["job_attempt_no"], 1)
        self.assertEqual(registered["scope"]["source_attempt_no"], 1)
        scope = {
            "projectId": "project-1", "novelWorkId": "project-work", "novelChapterId": "chapter-1",
            "sourceRevisionId": "revision-1", "productionJobId": "job-1", "sourceAnalysisRunId": "source-run-1",
        }
        output = run_source_gate(database, scope, self.temp / "source-prelink-scope.json")
        self.assertTrue(output["ok"])
        self.assertEqual(output["result"]["jobAttemptNo"], 1)
        self.assertEqual(output["result"]["sourceAttemptNo"], 1)

    def test_source_gate_rejects_old_or_cross_scope_or_inactive_prelink_candidates(self) -> None:
        content_hash = "sha256:" + "2" * 64
        scope = {
            "projectId": "project-1", "novelWorkId": "project-work", "novelChapterId": "chapter-1",
            "sourceRevisionId": "revision-1", "productionJobId": "job-1", "sourceAnalysisRunId": "source-run-1",
        }
        cases = {
            "old": ["UPDATE source_analysis_runs SET created_at=0"],
            "other_work": ["UPDATE novel_analysis_lineages SET novel_work_id='other-work'"],
            "other_revision": ["UPDATE source_analysis_runs SET novel_chapter_revision_id='other-revision'"],
            "other_adaptation": ["UPDATE source_analysis_runs SET frozen_comic_adaptation_id='other-adaptation'"],
            "wrong_key": ["UPDATE source_analysis_runs SET idempotency_key='old-job:source:1'"],
            "inactive_attempt": ["UPDATE source_analysis_run_attempts SET status='error'"],
            "expired_attempt": ["UPDATE source_analysis_run_attempts SET lease_expires_at=0"],
            "missing_owner": ["UPDATE source_analysis_run_attempts SET lease_owner=NULL"],
            "wrong_job_status": ["UPDATE novel_production_jobs SET status='error'"],
            "wrong_job_stage": ["UPDATE novel_production_jobs SET stage='integrating_context'"],
        }
        for name, statements in cases.items():
            with self.subTest(name=name):
                database = self.temp / f"source-reject-{name}.db"; make_scope_database(database, content_hash)
                conn = sqlite3.connect(database)
                for statement in statements: conn.execute(statement)
                conn.commit(); conn.close()
                output = run_source_gate(database, scope, self.temp / f"source-reject-{name}.json")
                self.assertFalse(output["ok"])

    def test_source_gate_rejects_multiple_live_runs_and_preserves_post_link_exactness(self) -> None:
        content_hash = "sha256:" + "3" * 64
        scope = {
            "projectId": "project-1", "novelWorkId": "project-work", "novelChapterId": "chapter-1",
            "sourceRevisionId": "revision-1", "productionJobId": "job-1", "sourceAnalysisRunId": "source-run-1",
        }
        database = self.temp / "source-multiple.db"; make_scope_database(database, content_hash)
        conn = sqlite3.connect(database)
        conn.execute("INSERT INTO source_analysis_runs VALUES('source-run-2','revision-1','lineage-1','adaptation-1','job-1:source:1','running',1000)")
        conn.execute("INSERT INTO source_analysis_run_attempts VALUES('attempt-2','source-run-2',1,'running','owner-2',4102444800000)")
        conn.commit(); conn.close()
        self.assertFalse(run_source_gate(database, scope, self.temp / "source-multiple.json")["ok"])

        database = self.temp / "source-post-link.db"; make_scope_database(database, content_hash)
        conn = sqlite3.connect(database)
        conn.execute("UPDATE novel_production_jobs SET source_analysis_run_id='source-run-1'")
        conn.commit(); conn.close()
        self.assertTrue(run_source_gate(database, scope, self.temp / "source-post-link.json")["ok"])
        conn = sqlite3.connect(database)
        conn.execute("UPDATE novel_production_jobs SET source_analysis_run_id='different-source-run'")
        conn.commit(); conn.close()
        self.assertFalse(run_source_gate(database, scope, self.temp / "source-post-link-wrong.json")["ok"])

    def test_adaptation_prelink_gate_binds_the_unique_current_frozen_run(self) -> None:
        content_hash = "sha256:" + "4" * 64
        database = self.temp / "adaptation-prelink.db"; make_scope_database(database, content_hash)
        activate_adaptation_prelink(database)
        scope = {
            "projectId": "project-1", "novelWorkId": "project-work", "novelChapterId": "chapter-1",
            "sourceRevisionId": "revision-1", "productionJobId": "job-1", "sourceAnalysisRunId": "source-run-1",
            "adaptationAnalysisRunId": "adaptation-run-1", "comicAdaptationId": "adaptation-1",
            "comicAdaptationChapterId": "adaptation-chapter-1",
        }
        output = run_adaptation_gate(database, scope, self.temp / "adaptation-prelink.json")
        self.assertTrue(output["ok"])
        self.assertEqual(output["result"], {
            "jobId": "job-1", "jobAttemptNo": 1, "sourceAnalysisRunId": "source-run-1",
            "adaptationAnalysisRunId": "adaptation-run-1", "adaptationAttemptNo": 1,
        })

    def test_adaptation_prelink_rejects_forged_old_or_cross_scope_runs(self) -> None:
        content_hash = "sha256:" + "5" * 64
        scope = {
            "projectId": "project-1", "novelWorkId": "project-work", "novelChapterId": "chapter-1",
            "sourceRevisionId": "revision-1", "productionJobId": "job-1", "sourceAnalysisRunId": "source-run-1",
            "adaptationAnalysisRunId": "adaptation-run-1", "comicAdaptationId": "adaptation-1",
            "comicAdaptationChapterId": "adaptation-chapter-1",
        }
        cases = {
            "old_attempt_key": ["UPDATE adaptation_analysis_runs SET idempotency_key='job-1:adaptation:0'"],
            "old_created_at": ["UPDATE adaptation_analysis_runs SET created_at=0"],
            "other_work": ["UPDATE adaptation_analysis_runs SET novel_work_id='other-work'"],
            "other_adaptation": ["UPDATE adaptation_analysis_runs SET comic_adaptation_id='other-adaptation'"],
            "other_chapter": ["UPDATE comic_adaptation_chapters SET novel_chapter_revision_id='other-revision'"],
            "other_source_lineage": ["UPDATE novel_analysis_lineages SET novel_work_id='other-work'"],
            "other_source_adaptation": ["UPDATE source_analysis_runs SET frozen_comic_adaptation_id='other-adaptation'"],
            "wrong_source_input": ["UPDATE analysis_artifacts SET source_analysis_run_id='old-source-run' WHERE id='source-artifact-0'"],
            "unadopted_input": ["UPDATE analysis_artifact_revisions SET status='candidate' WHERE id='source-revision-0'"],
            "reordered_input": ["UPDATE adaptation_analysis_run_inputs SET source_order=9 WHERE adaptation_analysis_run_id='adaptation-run-1' AND artifact_type='chapter_summary'"],
            "inactive_run": ["UPDATE adaptation_analysis_runs SET status='error'"],
            "inactive_attempt": ["UPDATE adaptation_analysis_run_attempts SET status='error'"],
            "expired_attempt": ["UPDATE adaptation_analysis_run_attempts SET lease_expires_at=0"],
            "missing_owner": ["UPDATE adaptation_analysis_runs SET lease_owner=NULL"],
            "empty_owner": ["UPDATE adaptation_analysis_runs SET lease_owner=''"],
            "mismatched_owner": ["UPDATE adaptation_analysis_run_attempts SET lease_owner='other-owner'"],
            "source_analysis_stage": ["UPDATE novel_production_jobs SET stage='source_analysis'"],
            "post_dispatch_stage": ["UPDATE novel_production_jobs SET stage='adaptation_analysis'"],
            "terminal_stage": ["UPDATE novel_production_jobs SET stage='succeeded',status='succeeded'"],
            "arbitrary_stage": ["UPDATE novel_production_jobs SET stage='not_a_production_stage'"],
            "wrong_job_status": ["UPDATE novel_production_jobs SET status='error'"],
        }
        for name, statements in cases.items():
            with self.subTest(name=name):
                database = self.temp / f"adaptation-reject-{name}.db"; make_scope_database(database, content_hash)
                activate_adaptation_prelink(database)
                conn = sqlite3.connect(database)
                for statement in statements: conn.execute(statement)
                conn.commit(); conn.close()
                self.assertFalse(run_adaptation_gate(database, scope, self.temp / f"adaptation-reject-{name}.json")["ok"])

    def test_adaptation_prelink_rejects_multiple_candidates_and_preserves_post_link_exactness(self) -> None:
        content_hash = "sha256:" + "6" * 64
        scope = {
            "projectId": "project-1", "novelWorkId": "project-work", "novelChapterId": "chapter-1",
            "sourceRevisionId": "revision-1", "productionJobId": "job-1", "sourceAnalysisRunId": "source-run-1",
            "adaptationAnalysisRunId": "adaptation-run-1", "comicAdaptationId": "adaptation-1",
            "comicAdaptationChapterId": "adaptation-chapter-1",
        }
        database = self.temp / "adaptation-multiple.db"; make_scope_database(database, content_hash)
        activate_adaptation_prelink(database)
        conn = sqlite3.connect(database)
        conn.execute("INSERT INTO adaptation_analysis_runs VALUES('adaptation-run-2','project-1','project-work','adaptation-1','adaptation-chapter-1','source_run','source-run-1','job-1:adaptation:1','running',1,'adapt-owner-2',4102444800000,1000)")
        conn.execute("INSERT INTO adaptation_analysis_run_attempts VALUES('adapt-attempt-2','adaptation-run-2',1,'running','adapt-owner-2',4102444800000)")
        conn.commit(); conn.close()
        self.assertFalse(run_adaptation_gate(database, scope, self.temp / "adaptation-multiple.json")["ok"])

        database = self.temp / "adaptation-post-link.db"; make_scope_database(database, content_hash)
        activate_adaptation_prelink(database)
        conn = sqlite3.connect(database)
        conn.execute("UPDATE novel_production_jobs SET adaptation_analysis_run_id='adaptation-run-1'")
        conn.commit(); conn.close()
        self.assertTrue(run_adaptation_gate(database, scope, self.temp / "adaptation-post-link.json")["ok"])
        conn = sqlite3.connect(database)
        conn.execute("UPDATE novel_production_jobs SET adaptation_analysis_run_id='different-adaptation-run'")
        conn.commit(); conn.close()
        self.assertFalse(run_adaptation_gate(database, scope, self.temp / "adaptation-post-link-wrong.json")["ok"])

    def test_manual_candidate_requires_exact_new_head_parent_and_version(self) -> None:
        database = self.temp / "manual.db"; make_manual_database(database)
        scope = self.temp / "scope.json"; before = self.temp / "before.json"
        scope.write_text(json.dumps({"schemaVersion":"normal-ui-acceptance-scope.v1","projectId":"project-1","novelWorkId":"work-1","novelChapterId":"chapter-1","sourceRevisionId":"revision-1","productionJobId":"job-1","sourceAnalysisRunId":"source-run-1"}), encoding="utf-8")
        helper = str(SCRIPT_ROOT / "verify_manual_candidate.py")
        initial = run_json(helper, "--database", str(database), "--scope", str(scope))
        self.assertTrue(initial["ok"]); before.write_text(json.dumps(initial["snapshot"]), encoding="utf-8")
        conn = sqlite3.connect(database)
        conn.execute("INSERT INTO analysis_artifact_revisions VALUES('revision-candidate-new','artifact-world','revision-candidate-old','candidate')")
        conn.execute("UPDATE analysis_artifacts SET candidate_head_revision_id='revision-candidate-new',optimistic_version=8 WHERE id='artifact-world'")
        conn.commit(); conn.close()
        after = run_json(helper, "--database", str(database), "--scope", str(scope), "--before", str(before))
        self.assertTrue(after["ok"])
        self.assertEqual(after["snapshot"]["candidateHeadRevisionId"], "revision-candidate-new")
        self.assertEqual(after["snapshot"]["candidateParents"]["revision-candidate-new"], "revision-candidate-old")

    def test_export_requires_one_receipted_file_with_the_candidate_asset_hash(self) -> None:
        database = self.temp / "export.db"; make_manual_database(database)
        source = self.temp / "candidate.png"; source.write_bytes(b"not-a-rendered-png-but-a-stable-fixture")
        data_root = self.temp / "data"; export_root = data_root / "漫画导出" / "work-1" / "production-chapter-1"
        directory = export_root / "comic-pages-export-1"; directory.mkdir(parents=True)
        manifest = directory / "manifest.json"; manifest.write_text("{}", encoding="utf-8")
        exported = directory / "001-page-1.png"; exported.write_bytes(source.read_bytes())
        # Rust canonicalize stores local Windows outputs with this prefix.
        extended = lambda path: "\\\\?\\" + str(path)
        conn = sqlite3.connect(database)
        conn.executescript("""
          CREATE TABLE novel_production_jobs(id TEXT PRIMARY KEY,project_id TEXT,novel_work_id TEXT,novel_chapter_id TEXT,source_revision_id TEXT,source_analysis_run_id TEXT);
          CREATE TABLE comic_visual_exports(id TEXT PRIMARY KEY,batch_id TEXT,project_id TEXT,novel_work_id TEXT,destination_dir TEXT,directory_path TEXT,manifest_path TEXT,status TEXT,selection_json TEXT);
          CREATE TABLE comic_visual_export_command_receipts(command_name TEXT,idempotency_key TEXT,request_hash TEXT,export_id TEXT,created_at INTEGER);
          CREATE TABLE comic_visual_export_files(export_id TEXT,member_id TEXT,ordinal INTEGER,manifest_id TEXT,production_chapter_id TEXT,production_page_id TEXT,run_id TEXT,asset_id TEXT,path TEXT,sha256 TEXT);
        """)
        conn.execute("INSERT INTO assets VALUES('asset-1',?)", (str(source),))
        conn.execute("INSERT INTO novel_production_jobs VALUES('job-1','project-1','work-1','chapter-1','revision-1','source-run-1')")
        selection = {"kind":"full_batch","candidate":True,"files":[{"memberId":"member-1","ordinal":1,"manifestId":"manifest-1","productionChapterId":"production-chapter-1","productionPageId":"production-page-1","runId":"page-run-1","assetId":"asset-1"}]}
        conn.execute("INSERT INTO comic_visual_exports VALUES('export-1','batch-1','project-1','work-1',?,?,?,'complete',?)", (extended(export_root),extended(directory),extended(manifest),json.dumps(selection)))
        conn.execute("INSERT INTO comic_visual_export_command_receipts VALUES('comic_visual_batch_export','key','hash','export-1',1)")
        digest = "sha256:" + hashlib.sha256(source.read_bytes()).hexdigest(); selection["files"][0]["sha256"] = digest
        conn.execute("INSERT INTO comic_visual_export_files VALUES('export-1','member-1',1,'manifest-1','production-chapter-1','production-page-1','page-run-1','asset-1',?,?)", (extended(exported),digest))
        conn.execute("UPDATE comic_visual_exports SET selection_json=? WHERE id='export-1'", (json.dumps(selection),))
        manifest.write_text(json.dumps(selection), encoding="utf-8")
        conn.commit(); conn.close()
        scope = self.temp / "export-scope.json"
        scope.write_text(json.dumps({"projectId":"project-1","novelWorkId":"work-1","novelChapterId":"chapter-1","sourceRevisionId":"revision-1","productionJobId":"job-1","sourceAnalysisRunId":"source-run-1"}), encoding="utf-8")
        before = self.temp / "export-before.json"
        snapshot = run_json(str(SCRIPT_ROOT / "verify_ui_export.py"), "--database", str(database), "--scope", str(scope), "--data-root", str(data_root), "--snapshot-out", str(before))
        self.assertTrue(snapshot["ok"]); self.assertTrue(before.is_file())
        output = run_json(str(SCRIPT_ROOT / "verify_ui_export.py"), "--database", str(database), "--scope", str(scope), "--data-root", str(data_root), "--before-candidate", str(before))
        self.assertTrue(output["ok"]); self.assertEqual(output["sha256"], digest)
        original_source = source.read_bytes(); source.write_bytes(b"candidate-changed-after-snapshot")
        changed_candidate = run_json(str(SCRIPT_ROOT / "verify_ui_export.py"), "--database", str(database), "--scope", str(scope), "--data-root", str(data_root), "--before-candidate", str(before))
        self.assertFalse(changed_candidate["ok"]); self.assertEqual(changed_candidate["code"], "UI_EXPORT_CANDIDATE_CHANGED")
        source.write_bytes(original_source)
        conn = sqlite3.connect(database); conn.execute("UPDATE comic_visual_exports SET destination_dir=? WHERE id='export-1'", (str(self.temp / "wrong-default"),)); conn.commit(); conn.close()
        wrong_default = run_json(str(SCRIPT_ROOT / "verify_ui_export.py"), "--database", str(database), "--scope", str(scope), "--data-root", str(data_root), "--before-candidate", str(before))
        self.assertFalse(wrong_default["ok"]); self.assertEqual(wrong_default["code"], "UI_EXPORT_NOT_COMPLETE")
        conn = sqlite3.connect(database); conn.execute("UPDATE comic_visual_exports SET destination_dir=? WHERE id='export-1'", (extended(export_root),)); conn.commit(); conn.close()
        conn = sqlite3.connect(database); conn.execute("UPDATE comic_visual_export_files SET sha256=? WHERE export_id='export-1'", (digest.removeprefix("sha256:"),)); conn.commit(); conn.close()
        wrong_prefix = run_json(str(SCRIPT_ROOT / "verify_ui_export.py"), "--database", str(database), "--scope", str(scope), "--data-root", str(data_root), "--before-candidate", str(before))
        self.assertFalse(wrong_prefix["ok"]); self.assertEqual(wrong_prefix["code"], "UI_EXPORT_MANIFEST_INVALID")
        conn = sqlite3.connect(database); conn.execute("UPDATE comic_visual_export_files SET sha256=?,manifest_id='wrong-manifest' WHERE export_id='export-1'", (digest,)); conn.commit(); conn.close()
        wrong_lineage = run_json(str(SCRIPT_ROOT / "verify_ui_export.py"), "--database", str(database), "--scope", str(scope), "--data-root", str(data_root), "--before-candidate", str(before))
        self.assertFalse(wrong_lineage["ok"]); self.assertEqual(wrong_lineage["code"], "UI_EXPORT_LINEAGE_INVALID")
        conn = sqlite3.connect(database); conn.execute("UPDATE comic_visual_export_files SET manifest_id='manifest-1' WHERE export_id='export-1'"); conn.execute("UPDATE assets SET path='C:\\Windows\\notepad.exe' WHERE id='asset-1'"); conn.commit(); conn.close()
        outside_d = run_json(str(SCRIPT_ROOT / "verify_ui_export.py"), "--database", str(database), "--scope", str(scope), "--data-root", str(data_root), "--before-candidate", str(before))
        self.assertFalse(outside_d["ok"]); self.assertEqual(outside_d["code"], "UI_EXPORT_VERIFY_FAILED")
        bad_scope = json.loads(scope.read_text(encoding="utf-8")); bad_scope["sourceAnalysisRunId"] = "wrong-source"; scope.write_text(json.dumps(bad_scope), encoding="utf-8")
        wrong_scope = run_json(str(SCRIPT_ROOT / "verify_ui_export.py"), "--database", str(database), "--scope", str(scope), "--data-root", str(data_root), "--before-candidate", str(before))
        self.assertFalse(wrong_scope["ok"]); self.assertEqual(wrong_scope["code"], "UI_EXPORT_BATCH_NOT_UNIQUE")


if __name__ == "__main__":
    unittest.main(verbosity=2)
