import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

const [auditRoot] = process.argv.slice(2);
assert(path.isAbsolute(auditRoot), 'Fixture root must be an absolute isolated audit directory');
assert(path.basename(auditRoot).startsWith('image-client-md-audit-'), 'Refuse to seed outside the dedicated audit namespace');
const supervisor = JSON.parse(await readFile(path.join(auditRoot, 'supervisor.json'), 'utf8'));
assert.equal(path.resolve(supervisor.dataDir), path.resolve(auditRoot, 'data'));
let priorAppAlive = false;
try { process.kill(supervisor.appPid, 0); priorAppAlive = true; } catch (error) { if (error.code !== 'ESRCH') throw error; }
assert.equal(priorAppAlive, false, 'Seed only after the supervisor has stopped its own app');
const checkpoint = JSON.parse(await readFile(path.join(auditRoot, 'checkpoint.json'), 'utf8'));
const scope = checkpoint.scope;
const mock = await fetch(`http://127.0.0.1:${supervisor.mockPort}/__state`).then(r => r.json());
const sortedJson = object => JSON.stringify(object, Object.keys(object).sort());
const hash = text => `sha256:${createHash('sha256').update(text).digest('hex')}`;
const db = new DatabaseSync(path.join(auditRoot, 'data', 'image-client.db'));
let evidence;
try {
  db.exec('PRAGMA foreign_keys=ON; BEGIN IMMEDIATE');
  const source = db.prepare('SELECT current_revision_id FROM novel_chapters WHERE id=? AND novel_work_id=?').get(scope.chapterId, scope.novelWorkId);
  assert(source?.current_revision_id);
  const other = db.prepare('SELECT id,current_revision_id FROM novel_chapters WHERE novel_work_id=? AND chapter_no=99').get(scope.novelWorkId);
  assert(other?.current_revision_id);
  const timestamp = Date.now();
  const jobId = 'retirement-audit-pending-job', batchId = 'retirement-audit-authorized-batch', completedId = 'retirement-audit-completed-job';
  const authorization = { model: 'gpt-image-2', providerId: 'gateway', size: '1024x1536', target: 'comic_pages' };
  const authorizationJson = sortedJson(authorization);
  const request = { projectId: scope.projectId, novelWorkId: scope.novelWorkId, novelChapterId: scope.chapterId, sourceRevisionId: source.current_revision_id, providerId: null, modelId: null };
  db.prepare(`INSERT INTO novel_production_jobs(id,project_id,novel_work_id,novel_chapter_id,source_revision_id,request_hash,idempotency_key,status,stage,stage_index,stage_total,completed_artifact_count,total_artifact_count,created_at,updated_at)
    VALUES(?,?,?,?,?,?,?,'queued','source_analysis',2,8,0,14,?,?)`).run(jobId, scope.projectId, scope.novelWorkId, scope.chapterId, source.current_revision_id, hash(sortedJson(request)), 'retirement-audit-pending-key', timestamp, timestamp);
  db.prepare(`INSERT INTO comic_visual_batches(id,project_id,novel_work_id,novel_chapter_id,source_revision_id,production_job_id,output_target,authorization_json,authorization_fingerprint,request_hash,idempotency_key,status,created_at,updated_at)
    VALUES(?,?,?,?,?,?,'comic_pages',?,?,?,?,'authorized',?,?)`).run(batchId, scope.projectId, scope.novelWorkId, scope.chapterId, source.current_revision_id, jobId, authorizationJson, hash(authorizationJson), hash(JSON.stringify({ authorization, productionJobId: jobId })), 'retirement-audit-batch-key', timestamp, timestamp);
  db.prepare(`INSERT INTO novel_production_jobs(id,project_id,novel_work_id,novel_chapter_id,source_revision_id,request_hash,idempotency_key,status,stage,stage_index,stage_total,completed_artifact_count,total_artifact_count,safe_user_message,created_at,updated_at,finished_at)
    VALUES(?,?,?,?,?,?,?,'succeeded','succeeded',8,8,14,14,'旧完成资料必须保留',?,?,?)`).run(completedId, scope.projectId, scope.novelWorkId, other.id, other.current_revision_id, 'completed-sentinel-hash', 'retirement-audit-completed-key', timestamp, timestamp, timestamp);
  assert.deepEqual(db.prepare('PRAGMA foreign_key_check').all(), [], 'Seed must satisfy real schema FKs');
  const recoveryCandidates = db.prepare(`SELECT job.id FROM novel_production_jobs job JOIN comic_visual_batches batch ON batch.production_job_id=job.id WHERE batch.status IN ('authorized','waiting_text','preparing','running','blocked_config')`).all();
  assert(recoveryCandidates.some(row => row.id === jobId), 'Pending fixture must match actual old recovery candidate SQL');
  evidence = {
    scope, providerRequestCount: mock.requests.length, recoveryCandidates,
    pending: db.prepare('SELECT * FROM novel_production_jobs WHERE id=?').get(jobId),
    batch: db.prepare('SELECT * FROM comic_visual_batches WHERE id=?').get(batchId),
    completed: db.prepare('SELECT * FROM novel_production_jobs WHERE id=?').get(completedId),
    sourceContent: db.prepare('SELECT content FROM novel_chapter_revisions WHERE id=?').get(source.current_revision_id).content,
    sourceAnalysisCount: db.prepare('SELECT COUNT(*) AS count FROM source_analysis_runs').get().count,
    adaptationAnalysisCount: db.prepare('SELECT COUNT(*) AS count FROM adaptation_analysis_runs').get().count,
  };
  db.exec('COMMIT');
} catch (error) { try { db.exec('ROLLBACK'); } catch {} throw error; }
finally { db.close(); }
await writeFile(path.join(auditRoot, 'legacy-retirement-checkpoint.json'), JSON.stringify(evidence, null, 2));
console.log('Seeded valid old queued production + authorized visual batch and completed-history sentinel while own app was stopped.');
