import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import test from 'node:test';
import { DatabaseSync } from 'node:sqlite';

import { NormalUiAcceptanceProxy } from '../normal_ui_proxy.mjs';

const ROOT = 'D:\\cc\\image-client\\.test-tmp';

function startServer(handler) {
  const server = createServer(handler);
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      resolve({ server, port: address.port });
    });
  });
}

function responseJson(url, body, token = 'test-private-token') {
  return fetch(url, {
    method: 'POST',
    redirect: 'manual',
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
}

function createDatabase(path) {
  const db = new DatabaseSync(path);
  db.exec(`
    CREATE TABLE novel_production_jobs(id TEXT,project_id TEXT,novel_work_id TEXT,novel_chapter_id TEXT,source_revision_id TEXT,default_adaptation_id TEXT,source_analysis_run_id TEXT,adaptation_analysis_run_id TEXT,apply_operation_id TEXT,status TEXT,stage TEXT,comic_plan_intent_json TEXT,attempt_no INTEGER,created_at INTEGER);
    CREATE TABLE novel_analysis_lineages(id TEXT,novel_work_id TEXT);
    CREATE TABLE source_analysis_runs(id TEXT,status TEXT,novel_chapter_revision_id TEXT,novel_analysis_lineage_id TEXT,frozen_comic_adaptation_id TEXT,idempotency_key TEXT,created_at INTEGER);
    CREATE TABLE source_analysis_run_attempts(id TEXT,source_analysis_run_id TEXT,attempt_no INTEGER,status TEXT,lease_owner TEXT,lease_expires_at INTEGER);
    CREATE TABLE comic_adaptation_chapters(id TEXT,comic_adaptation_id TEXT,novel_chapter_revision_id TEXT);
    CREATE TABLE adaptation_analysis_runs(id TEXT,status TEXT,project_id TEXT,novel_work_id TEXT,comic_adaptation_id TEXT,comic_adaptation_chapter_id TEXT,input_mode TEXT,source_analysis_run_id TEXT,idempotency_key TEXT,attempt_no INTEGER,lease_owner TEXT,lease_expires_at INTEGER,created_at INTEGER);
    CREATE TABLE adaptation_analysis_run_attempts(id TEXT,adaptation_analysis_run_id TEXT,attempt_no INTEGER,status TEXT,lease_owner TEXT,lease_expires_at INTEGER);
    CREATE TABLE analysis_artifacts(id TEXT,source_analysis_run_id TEXT,artifact_type TEXT,adopted_head_revision_id TEXT);
    CREATE TABLE analysis_artifact_revisions(id TEXT,analysis_artifact_id TEXT,status TEXT);
    CREATE TABLE adaptation_analysis_run_inputs(adaptation_analysis_run_id TEXT,artifact_type TEXT,analysis_artifact_revision_id TEXT,source_order INTEGER);
    CREATE TABLE analysis_apply_operations(id TEXT,operation_type TEXT,status TEXT);
    CREATE TABLE comic_visual_batches(id TEXT,production_job_id TEXT,project_id TEXT,novel_work_id TEXT,comic_adaptation_id TEXT,apply_operation_id TEXT,status TEXT,total_members INTEGER);
    CREATE TABLE comic_visual_batch_members(id TEXT,batch_id TEXT,ordinal INTEGER,manifest_id TEXT,production_chapter_id TEXT,production_page_id TEXT,page_run_id TEXT,status TEXT);
    CREATE TABLE comic_visual_manifests(id TEXT,project_id TEXT,novel_work_id TEXT,comic_adaptation_id TEXT,production_chapter_id TEXT,contract_version TEXT,manifest_fingerprint TEXT,manifest_json TEXT,apply_operation_id TEXT);
    CREATE TABLE comic_visual_page_runs(id TEXT,manifest_id TEXT,production_page_id TEXT,compiler_contract_version TEXT,status TEXT);
    CREATE TABLE comic_production_pages(id TEXT,comic_production_chapter_id TEXT);
  `);
  db.prepare('INSERT INTO novel_analysis_lineages VALUES (?,?)').run('lineage', 'work');
  db.prepare('INSERT INTO source_analysis_runs VALUES (?,?,?,?,?,?,?)').run('source-run', 'running', 'revision', 'lineage', 'adaptation', 'job:source:1', 1000);
  db.prepare('INSERT INTO source_analysis_run_attempts VALUES (?,?,?,?,?,?)').run('source-attempt', 'source-run', 1, 'running', 'owner', 4102444800000);
  db.prepare('INSERT INTO comic_adaptation_chapters VALUES (?,?,?)').run('adapt-chapter', 'adaptation', 'revision');
  db.prepare('INSERT INTO adaptation_analysis_runs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)').run(
    'adapt-run', 'running', 'project', 'work', 'adaptation', 'adapt-chapter', 'artifact_revisions', null,
    'job:adaptation:1', 1, 'adapt-owner', 4102444800000, 1000,
  );
  db.prepare('INSERT INTO adaptation_analysis_run_attempts VALUES (?,?,?,?,?,?)').run('adapt-attempt', 'adapt-run', 1, 'running', 'adapt-owner', 4102444800000);
  const sourceTypes = ['chapter_summary', 'chapter_beats', 'world_facts', 'character_facts', 'faction_facts', 'location_facts', 'prop_facts', 'timeline_delta', 'continuity_delta', 'open_threads'];
  for (const [index, artifactType] of sourceTypes.entries()) {
    const artifactId = `source-artifact-${index}`;
    const revisionId = `source-revision-${index}`;
    db.prepare('INSERT INTO analysis_artifacts VALUES (?,?,?,?)').run(artifactId, 'source-run', artifactType, revisionId);
    db.prepare('INSERT INTO analysis_artifact_revisions VALUES (?,?,?)').run(revisionId, artifactId, 'adopted');
    db.prepare('INSERT INTO adaptation_analysis_run_inputs VALUES (?,?,?,?)').run('adapt-run', artifactType, revisionId, index);
  }
  db.prepare('INSERT INTO novel_production_jobs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)').run(
    'job', 'project', 'work', 'chapter', 'revision', 'adaptation', 'source-run', null, 'apply', 'running', 'source_analysis',
    JSON.stringify({ pages: [{ panelCount: 5, layoutProfile: 'hero_middle_5' }] }), 1, 1,
  );
  db.prepare('INSERT INTO analysis_apply_operations VALUES (?,?,?)').run('apply', 'apply_comic_plan', 'succeeded');
  db.close();
}

function createMigratedEmptyDatabase(path) {
  const db = new DatabaseSync(path);
  db.exec('PRAGMA foreign_keys=ON;');
  const migrations = readdirSync(resolve('src-tauri/migrations'))
    .filter((name) => /^\d{4}_.+\.sql$/.test(name))
    .sort();
  for (const migration of migrations) {
    db.exec(readFileSync(resolve('src-tauri/migrations', migration), 'utf8'));
  }
  db.close();
}

function readOnlyGate(databasePath, root, kind) {
  const scopePath = join(root, `migrated-${kind}-scope.json`);
  writeFileSync(scopePath, JSON.stringify({
    schemaVersion: 'normal-ui-acceptance-scope.v1', projectId: 'project', novelWorkId: 'work', novelChapterId: 'chapter', sourceRevisionId: 'revision',
    productionJobId: 'job', sourceAnalysisRunId: 'source-run', adaptationAnalysisRunId: 'adapt-run', comicAdaptationId: 'adaptation', comicAdaptationChapterId: 'adapt-chapter',
  }));
  const result = spawnSync('python', ['scripts/normal-ui-acceptance/db_gate.py', '--database', databasePath, '--kind', kind, '--scope', scopePath], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout);
}

function makeLlm(kind) {
  if (kind === 'source') {
    return {
      model: 'test-llm', stream: true, stream_options: { include_usage: true },
      messages: [
        { role: 'system', content: '你是小说分析器。仅输出一个聚合 JSON：{"artifacts":[...14 items...]}，不得 Markdown 或解释。artifacts 必须恰有 14 项、每种 artifactType 一项；每一项都须分别符合下列冻结 Schema root oneOf，包含 schemaVersion/owner/content/warnings。不得生成 canon_diff；它只能由已采用 canon delta 确定性派生。\nnovel-analysis.v1' },
        { role: 'user', content: JSON.stringify({ chapterContent: '正文', chapterContentHash: 'hash', ownerMap: { novel_work: 'work', novel_chapter_revision: 'revision' } }) },
      ],
    };
  }
  return {
    model: 'test-llm', stream: true, stream_options: { include_usage: true },
    messages: [
      { role: 'system', content: '你是改编规划分析器。只输出聚合 JSON {"artifacts":[...4 items...]}，没有 Markdown 或解释。artifacts 必须恰有 adaptation_proposal、comic_chapter_plan、scene_plan、page_panel_plan 各一项，绝不能输出十类原著分析。每项必须包含 schemaVersion=\'novel-analysis.v1\'、artifactType、owner、content、warnings；ownerMap 是强制精确值。每项 content 必须符合下面冻结的 Novel Analysis v1 Schema 相应 oneOf，page_panel_plan 还必须有可验证的完整 geometry。正式页面生产合同 outputIdentityBindings' },
      { role: 'user', content: JSON.stringify({ runId: 'adapt-run', novelWorkId: 'work', frozenOriginalArtifacts: Array.from({ length: 10 }, (_, index) => ({ index })), comicPlanIntent: { pages: [{ panelCount: 5 }] }, ownerMap: { comic_adaptation: 'adaptation', comic_chapter: 'adapt-chapter' } }) },
    ],
  };
}

function heroLayout() {
  const boxes = [[0.04, 0.04, 0.42, 0.2], [0.5, 0.04, 0.46, 0.2], [0.04, 0.27, 0.92, 0.38], [0.04, 0.68, 0.4, 0.24], [0.48, 0.68, 0.48, 0.24]];
  return {
    templateId: 'hero_middle_5', layoutKind: 'template', readingOrder: [1, 2, 3, 4, 5], dominantPanel: 3,
    geometry: {
      panelCount: 5, safeArea: { x: 0.04, y: 0.04, width: 0.92, height: 0.92 },
      panels: boxes.map(([x, y, width, height], index) => {
        const right = x + width;
        const polygon = index === 0
          ? [{ x, y }, { x: right, y }, { x: right - 0.02, y: y + height }, { x, y: y + height }]
          : index === 1
            ? [{ x, y }, { x: right, y }, { x: right, y: y + height }, { x: x + 0.02, y: y + height }]
            : [{ x, y }, { x: right, y }, { x: right, y: y + height }, { x, y: y + height }];
        return { panelNo: index + 1, bounds: { x, y, width, height }, polygon, textZone: { x, y, width, height } };
      }),
    },
  };
}

function activateImage(dbPath) {
  const page = {
    productionPageId: 'page',
    layout: heroLayout(),
    panels: Array.from({ length: 5 }, (_, index) => ({
      panelNo: index + 1,
      spec: {
        characterKeys: index === 0 ? ['甲'] : [],
        action: `动作${index + 1}`,
        visualBeat: `视觉节拍${index + 1}`,
        shot: index === 0 ? 'medium' : 'wide',
        narrativeFunction: index === 0 ? 'setup' : 'progression',
        dialogues: index === 0 ? [{ speaker: '甲', text: '短对白' }] : [],
        soundEffects: index === 2 ? ['沙沙'] : [],
      },
    })),
  };
  const db = new DatabaseSync(dbPath);
  db.prepare("UPDATE source_analysis_runs SET status='ready_for_review' WHERE id='source-run'").run();
  db.prepare("UPDATE adaptation_analysis_runs SET status='ready_for_review' WHERE id='adapt-run'").run();
  db.prepare("UPDATE novel_production_jobs SET adaptation_analysis_run_id='adapt-run',status='succeeded',stage='succeeded' WHERE id='job'").run();
  db.prepare('INSERT INTO comic_production_pages VALUES (?,?)').run('page', 'production-chapter');
  db.prepare('INSERT INTO comic_visual_manifests VALUES (?,?,?,?,?,?,?,?,?)').run('manifest', 'project', 'work', 'adaptation', 'production-chapter', 'comic-visual.v1', 'fingerprint', JSON.stringify({ pages: [page] }), 'apply');
  db.prepare('INSERT INTO comic_visual_page_runs VALUES (?,?,?,?,?)').run('page-run', 'manifest', 'page', 'comic-page-compiler.v3', 'running');
  db.prepare('INSERT INTO comic_visual_batches VALUES (?,?,?,?,?,?,?,?)').run('batch', 'job', 'project', 'work', 'adaptation', 'apply', 'running', 1);
  db.prepare('INSERT INTO comic_visual_batch_members VALUES (?,?,?,?,?,?,?,?)').run('member', 'batch', 1, 'manifest', 'production-chapter', 'page', 'page-run', 'running');
  db.close();
}

function activateAdaptation(dbPath) {
  const db = new DatabaseSync(dbPath);
  db.prepare("UPDATE source_analysis_runs SET status='ready_for_review' WHERE id='source-run'").run();
  db.prepare("UPDATE adaptation_analysis_runs SET status='running' WHERE id='adapt-run'").run();
  db.prepare("UPDATE novel_production_jobs SET adaptation_analysis_run_id=NULL,status='running',stage='ensuring_adaptation' WHERE id='job'").run();
  db.close();
}

async function fixture(t, upstreamHandler, proxyOptions = {}) {
  mkdirSync(ROOT, { recursive: true });
  const root = mkdtempSync(join(ROOT, 'normal-ui-proxy-test-'));
  const dbPath = join(root, 'image-client.db');
  const auditPath = join(root, 'audit.jsonl');
  const scopePath = join(root, 'scope.json');
  const checkpointPath = join(root, 'checkpoint.json');
  const releasePath = join(root, 'release.json');
  const readyPath = join(root, 'ready.json');
  createDatabase(dbPath);
  writeFileSync(scopePath, JSON.stringify({
    schemaVersion: 'normal-ui-acceptance-scope.v1', projectId: 'project', novelWorkId: 'work', novelChapterId: 'chapter', sourceRevisionId: 'revision',
    productionJobId: 'job', sourceAnalysisRunId: 'source-run',
  }));
  const upstream = await startServer(upstreamHandler);
  const envPath = join(root, 'real.env');
  writeFileSync(envPath, `LLM_API_URL=http://127.0.0.1:${upstream.port}/v1\nLLM_API_KEY=secret\nLLM_API_MODEL=test-llm\nIMAGE_API_URL=http://127.0.0.1:${upstream.port}/v1/images/generations\nIMAGE_API_KEY=secret\nIMAGE_API_MODEL=test-image\n`);
  const proxy = new NormalUiAcceptanceProxy({
    runtimeRoot: root, databasePath: dbPath, auditPath, scopePath, checkpointPath, releasePath, readyPath,
    realEnvPath: envPath, token: 'test-private-token', pythonPath: 'python', ...proxyOptions,
  });
  const port = await proxy.listen();
  t.after(async () => { await proxy.close(); await new Promise((resolve) => upstream.server.close(resolve)); rmSync(root, { recursive: true, force: true }); });
  return { root, dbPath, auditPath, checkpointPath, releasePath, scopePath, port };
}

async function waitForFile(path, timeoutMs = 2_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try { return JSON.parse(readFileSync(path, 'utf8')); } catch { await new Promise((resolveWait) => setTimeout(resolveWait, 25)); }
  }
  throw new Error(`timed out waiting for ${path}`);
}

async function waitForExit(child, timeoutMs = 2_000) {
  if (child.exitCode !== null) return;
  await Promise.race([
    new Promise((resolveExit) => child.once('exit', resolveExit)),
    new Promise((_, rejectExit) => setTimeout(() => rejectExit(new Error('child did not exit')), timeoutMs)),
  ]);
}

test('read-only gates prepare against the real v1-v21 migrated empty schema', (t) => {
  mkdirSync(ROOT, { recursive: true });
  const root = mkdtempSync(join(ROOT, 'normal-ui-proxy-migrated-'));
  const databasePath = join(root, 'image-client.db');
  t.after(() => rmSync(root, { recursive: true, force: true }));
  createMigratedEmptyDatabase(databasePath);
  for (const kind of ['source', 'adaptation', 'image']) {
    const result = readOnlyGate(databasePath, root, kind);
    assert.equal(result.ok, false);
    assert.match(result.code, new RegExp(`^UI_PROXY_${kind.toUpperCase()}_SCOPE_NOT_UNIQUE_NONE$`));
  }
});

test('proxy CLI accepts its private token only from child environment and reaches ready without forwarding', async (t) => {
  mkdirSync(ROOT, { recursive: true });
  const root = mkdtempSync(join(ROOT, 'normal-ui-proxy-cli-'));
  const databasePath = join(root, 'image-client.db');
  const configPath = join(root, 'proxy-config.json');
  const readyPath = join(root, 'proxy-ready.json');
  const envPath = join(root, 'real.env');
  createMigratedEmptyDatabase(databasePath);
  writeFileSync(envPath, 'LLM_API_URL=http://127.0.0.1:9/v1\nLLM_API_KEY=secret\nIMAGE_API_URL=http://127.0.0.1:9/v1/images/generations\nIMAGE_API_KEY=secret\n');
  writeFileSync(configPath, JSON.stringify({
    runtimeRoot: root, databasePath, auditPath: join(root, 'audit.jsonl'), scopePath: join(root, 'scope.json'),
    checkpointPath: join(root, 'checkpoint.json'), releasePath: join(root, 'release.json'), readyPath,
  }));
  const child = spawn(process.execPath, [resolve('scripts/normal-ui-acceptance/normal_ui_proxy.mjs'), '--config', configPath, '--real-env', envPath, '--port', '0'], {
    cwd: root, windowsHide: true, env: { ...process.env, UI_PROXY_TOKEN: 'main-only-test-token' }, stdio: ['ignore', 'ignore', 'pipe'],
  });
  let safeCliError = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => { safeCliError += chunk; });
  t.after(async () => {
    if (child.exitCode === null) child.kill('SIGTERM');
    await waitForExit(child);
    rmSync(root, { recursive: true, force: true });
  });
  let ready;
  try { ready = await waitForFile(readyPath); } catch (error) { assert.fail(`proxy CLI did not become ready: ${safeCliError.trim()}`); }
  assert.equal(ready.schemaVersion, 'normal-ui-acceptance-proxy.v1');
  assert.equal(typeof ready.port, 'number');
  assert.ok(ready.port > 0);
  assert.equal(existsSync(join(root, 'audit.jsonl')), false);
});

test('scope v1 freezes the job and source-run IDs before either text request can forward', async (t) => {
  let rejectedScopeForwards = 0;
  const noForward = (_req, res) => {
    rejectedScopeForwards += 1;
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.end('data: [DONE]\n\n');
  };
  const missingFx = await fixture(t, noForward);
  const missingScope = JSON.parse(readFileSync(missingFx.scopePath, 'utf8'));
  delete missingScope.sourceAnalysisRunId;
  writeFileSync(missingFx.scopePath, JSON.stringify(missingScope));
  let response = await responseJson(`http://127.0.0.1:${missingFx.port}/v1/chat/completions`, makeLlm('source'));
  assert.equal(response.status, 502);
  assert.equal(rejectedScopeForwards, 0);

  const mismatchFx = await fixture(t, noForward);
  const mismatchScope = JSON.parse(readFileSync(mismatchFx.scopePath, 'utf8'));
  writeFileSync(mismatchFx.scopePath, JSON.stringify({ ...mismatchScope, sourceAnalysisRunId: 'different-source-run' }));
  response = await responseJson(`http://127.0.0.1:${mismatchFx.port}/v1/chat/completions`, makeLlm('source'));
  assert.equal(response.status, 502);
  assert.equal(rejectedScopeForwards, 0);

  let calls = 0;
  const fx = await fixture(t, (_req, res) => {
    calls += 1;
    noForward(_req, res);
  });
  const sourceUrl = `http://127.0.0.1:${fx.port}/v1/chat/completions`;
  response = await responseJson(sourceUrl, makeLlm('source'));
  assert.equal(response.status, 200, await response.text());
  assert.equal(calls, 1);

  const bindingPath = join(fx.root, 'scope-binding.json');
  const binding = JSON.parse(readFileSync(bindingPath, 'utf8'));
  writeFileSync(bindingPath, JSON.stringify({ ...binding, sourceAnalysisRunId: 'different-source-run' }));
  activateAdaptation(fx.dbPath);
  response = await responseJson(sourceUrl, makeLlm('adaptation'));
  assert.equal(response.status, 502);
  assert.equal(calls, 1);
});

test('normal UI proxy gates source/adaptation, holds image, and forwards exactly released inline image', async (t) => {
  const calls = { source: 0, adaptation: 0, image: 0 };
  const fx = await fixture(t, (req, res) => {
    assert.equal(req.headers['accept-encoding'], 'identity');
    if (req.url === '/v1/chat/completions') {
      const chunks = [];
      req.on('data', (chunk) => chunks.push(chunk));
      req.on('end', () => {
        const body = JSON.parse(Buffer.concat(chunks));
        const system = body.messages[0].content;
        if (system.includes('小说分析器')) calls.source += 1; else calls.adaptation += 1;
        res.writeHead(200, { 'content-type': 'text/event-stream' });
        res.end('data: {"choices":[{"delta":{"content":"{}"}}]}\n\ndata: [DONE]\n\n');
      });
    } else if (req.url === '/v1/images/generations') {
      calls.image += 1;
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ data: [{ b64_json: 'aGVsbG8=' }] }));
    } else res.writeHead(404).end();
  });
  const base = `http://127.0.0.1:${fx.port}`;
  let response = await responseJson(`${base}/v1/chat/completions`, makeLlm('source'));
  assert.equal(response.status, 200);
  assert.match(await response.text(), /\[DONE\]/);
  assert.equal(calls.source, 1);

  activateAdaptation(fx.dbPath);
  response = await responseJson(`${base}/v1/chat/completions`, makeLlm('adaptation'));
  assert.equal(response.status, 200, await response.text());
  assert.equal(calls.adaptation, 1);

  activateImage(fx.dbPath);

  const image = { model: 'test-image', prompt: 'compiled prompt', n: 1, size: '1024x1536' };
  const imagePending = responseJson(`${base}/v1/images/generations`, image);
  const checkpoint = await waitForFile(fx.checkpointPath);
  assert.equal(calls.image, 0);
  assert.ok(Number.isFinite(Date.parse(checkpoint.checkpointCreatedAtUtc)));
  assert.equal(Date.parse(checkpoint.reviewExpiresAtUtc) - Date.parse(checkpoint.checkpointCreatedAtUtc), 180_000);
  assert.deepEqual(checkpoint.formalManifestReview.textByPanel[0], {
    panelNo: 1,
    characterKeys: ['甲'],
    action: '动作1',
    visualBeat: '视觉节拍1',
    shot: 'medium',
    narrativeFunction: 'setup',
    dialogues: [{ speaker: '甲', text: '短对白' }],
    captions: [],
    soundEffects: [],
  });
  writeFileSync(fx.releasePath, JSON.stringify({ schemaVersion: 'normal-ui-image-release.v1', jobId: checkpoint.jobId, manifestId: checkpoint.manifestId, manifestFingerprint: checkpoint.manifestFingerprint, requestDigest: checkpoint.requestDigest }));
  response = await imagePending;
  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), { data: [{ b64_json: 'aGVsbG8=' }] });
  assert.equal(calls.image, 1);
  assert.deepEqual(Object.keys(JSON.parse(readFileSync(fx.releasePath, 'utf8'))).sort(), ['jobId', 'manifestFingerprint', 'manifestId', 'requestDigest', 'schemaVersion']);
  response = await responseJson(`${base}/v1/images/generations`, image);
  assert.equal(response.status, 502);
  assert.equal(calls.image, 1);
  const events = readFileSync(fx.auditPath, 'utf8').trim().split(/\r?\n/).map(JSON.parse);
  for (const kind of ['source', 'adaptation', 'image']) {
    assert.equal(events.filter((event) => event.event === 'generation_post_reserved' && event.kind === kind).length, 1);
  }
  const sourceReservation = events.find((event) => event.event === 'generation_post_reserved' && event.kind === 'source');
  assert.deepEqual({
    jobId: sourceReservation.jobId,
    sourceAnalysisRunId: sourceReservation.sourceAnalysisRunId,
    jobAttemptNo: sourceReservation.jobAttemptNo,
    sourceAttemptNo: sourceReservation.sourceAttemptNo,
    hasScopeDigest: typeof sourceReservation.scopeDigest === 'string' && sourceReservation.scopeDigest.startsWith('sha256:'),
  }, {
    jobId: 'job', sourceAnalysisRunId: 'source-run', jobAttemptNo: 1, sourceAttemptNo: 1, hasScopeDigest: true,
  });
  const adaptationReservation = events.find((event) => event.event === 'generation_post_reserved' && event.kind === 'adaptation');
  assert.deepEqual({
    jobId: adaptationReservation.jobId,
    sourceAnalysisRunId: adaptationReservation.sourceAnalysisRunId,
    adaptationAnalysisRunId: adaptationReservation.adaptationAnalysisRunId,
    jobAttemptNo: adaptationReservation.jobAttemptNo,
    adaptationAttemptNo: adaptationReservation.adaptationAttemptNo,
    hasScopeDigest: typeof adaptationReservation.scopeDigest === 'string' && adaptationReservation.scopeDigest.startsWith('sha256:'),
  }, {
    jobId: 'job', sourceAnalysisRunId: 'source-run', adaptationAnalysisRunId: 'adapt-run',
    jobAttemptNo: 1, adaptationAttemptNo: 1, hasScopeDigest: true,
  });
});

test('short review window expires without image forward and a late release cannot revive reconciled work', async (t) => {
  let imageCalls = 0;
  const fx = await fixture(t, (req, res) => {
    if (req.url === '/v1/images/generations') imageCalls += 1;
    res.writeHead(200, { 'content-type': req.url === '/v1/images/generations' ? 'application/json' : 'text/event-stream' });
    res.end(req.url === '/v1/images/generations' ? JSON.stringify({ data: [{ b64_json: 'aGVsbG8=' }] }) : 'data: [DONE]\n\n');
  }, { imageReviewWaitMs: 75 });
  const base = `http://127.0.0.1:${fx.port}`;
  assert.equal((await responseJson(`${base}/v1/chat/completions`, makeLlm('source'))).status, 200);
  activateAdaptation(fx.dbPath);
  assert.equal((await responseJson(`${base}/v1/chat/completions`, makeLlm('adaptation'))).status, 200);
  activateImage(fx.dbPath);
  const image = { model: 'test-image', prompt: 'compiled prompt', n: 1, size: '1024x1536' };
  const timedOut = await responseJson(`${base}/v1/images/generations`, image);
  assert.equal(timedOut.status, 502);
  assert.equal(imageCalls, 0);
  const checkpoint = await waitForFile(fx.checkpointPath);
  assert.equal(Date.parse(checkpoint.reviewExpiresAtUtc) - Date.parse(checkpoint.checkpointCreatedAtUtc), 75);
  // A same-scope retry must inherit the expired checkpoint rather than quietly
  // opening another review window or consuming a second image reservation.
  const secondTimedOut = await responseJson(`${base}/v1/images/generations`, image);
  assert.equal(secondTimedOut.status, 502);
  const unchanged = await waitForFile(fx.checkpointPath);
  assert.equal(unchanged.checkpointCreatedAtUtc, checkpoint.checkpointCreatedAtUtc);
  assert.equal(unchanged.reviewExpiresAtUtc, checkpoint.reviewExpiresAtUtc);
  assert.equal(imageCalls, 0);
  const reconciled = new DatabaseSync(fx.dbPath);
  reconciled.prepare("UPDATE comic_visual_batches SET status='needs_reconcile'").run();
  reconciled.prepare("UPDATE comic_visual_batch_members SET status='needs_reconcile'").run();
  reconciled.prepare("UPDATE comic_visual_page_runs SET status='needs_reconcile'").run();
  reconciled.close();
  writeFileSync(fx.releasePath, JSON.stringify({ schemaVersion: 'normal-ui-image-release.v1', jobId: checkpoint.jobId, manifestId: checkpoint.manifestId, manifestFingerprint: checkpoint.manifestFingerprint, requestDigest: checkpoint.requestDigest }));
  const late = await responseJson(`${base}/v1/images/generations`, image);
  assert.equal(late.status, 502);
  assert.equal(imageCalls, 0);
  const audit = existsSync(fx.auditPath) ? readFileSync(fx.auditPath, 'utf8') : '';
  assert.doesNotMatch(audit, /generation_post_reserved.*image/);
});

test('deny paths, concurrent duplicate, 400 fallback and redirect never make a second forward', async (t) => {
  let calls = 0;
  const fx = await fixture(t, (req, res) => {
    calls += 1;
    res.writeHead(calls === 1 ? 422 : 302, { location: 'http://bad.example/' });
    res.end('{"error":"ignored"}');
  });
  const url = `http://127.0.0.1:${fx.port}/v1/chat/completions`;
  const [left, right] = await Promise.all([responseJson(url, makeLlm('source')), responseJson(url, makeLlm('source'))]);
  assert.deepEqual([left.status, right.status].sort(), [502, 502]);
  assert.equal(calls, 1);
  const fallback = makeLlm('source');
  delete fallback.stream;
  delete fallback.stream_options;
  const response = await responseJson(url, fallback);
  assert.equal(response.status, 502);
  assert.equal(calls, 1);
});

test('image SFX gate and URL-only upstream response fail closed without uncontrolled download', async (t) => {
  let imageCalls = 0;
  const fx = await fixture(t, (req, res) => {
    if (req.url === '/v1/images/generations') {
      imageCalls += 1;
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ data: [{ url: 'http://not-forwarded.invalid/asset.png' }] }));
    } else {
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.end('data: [DONE]\n\n');
    }
  });
  const source = await responseJson(`http://127.0.0.1:${fx.port}/v1/chat/completions`, makeLlm('source'));
  assert.equal(source.status, 200);
  const db = new DatabaseSync(fx.dbPath);
  const manifest = { pages: [{ productionPageId: 'page', layout: heroLayout(), panels: Array.from({ length: 5 }, (_, index) => ({ panelNo: index + 1, spec: { dialogues: [{ text: '短' }], soundEffects: [] } })) }] };
  db.prepare('INSERT INTO comic_production_pages VALUES (?,?)').run('page', 'production-chapter');
  db.prepare('INSERT INTO comic_visual_manifests VALUES (?,?,?,?,?,?,?,?,?)').run('manifest', 'project', 'work', 'adaptation', 'production-chapter', 'comic-visual.v1', 'fingerprint', JSON.stringify(manifest), 'apply');
  db.prepare('INSERT INTO comic_visual_page_runs VALUES (?,?,?,?,?)').run('page-run', 'manifest', 'page', 'comic-page-compiler.v3', 'running');
  db.prepare('INSERT INTO comic_visual_batches VALUES (?,?,?,?,?,?,?,?)').run('batch', 'job', 'project', 'work', 'adaptation', 'apply', 'running', 1);
  db.prepare('INSERT INTO comic_visual_batch_members VALUES (?,?,?,?,?,?,?,?)').run('member', 'batch', 1, 'manifest', 'production-chapter', 'page', 'page-run', 'running');
  db.prepare("UPDATE source_analysis_runs SET status='ready_for_review'").run();
  db.prepare("UPDATE adaptation_analysis_runs SET status='ready_for_review'").run();
  db.prepare("UPDATE novel_production_jobs SET adaptation_analysis_run_id='adapt-run',status='succeeded',stage='succeeded'").run();
  db.close();
  const image = { model: 'test-image', prompt: 'compiled prompt', n: 1, size: '1024x1536' };
  let response = await responseJson(`http://127.0.0.1:${fx.port}/v1/images/generations`, image);
  assert.equal(response.status, 502);
  assert.equal(imageCalls, 0);
  assert.doesNotMatch(readFileSync(fx.auditPath, 'utf8'), /generation_post_reserved.*image/);

  manifest.pages[0].panels[2].spec.soundEffects = ['砰'];
  const repaired = new DatabaseSync(fx.dbPath);
  repaired.prepare('UPDATE comic_visual_manifests SET manifest_json=? WHERE id=?').run(JSON.stringify(manifest), 'manifest');
  repaired.close();
  const pending = responseJson(`http://127.0.0.1:${fx.port}/v1/images/generations`, image);
  await new Promise((resolveWait) => setTimeout(resolveWait, 100));
  if (!existsSync(fx.checkpointPath)) {
    const rejected = await pending;
    assert.fail(await rejected.text());
  }
  const checkpoint = await waitForFile(fx.checkpointPath);
  writeFileSync(fx.releasePath, JSON.stringify({ schemaVersion: 'normal-ui-image-release.v1', jobId: checkpoint.jobId, manifestId: checkpoint.manifestId, manifestFingerprint: checkpoint.manifestFingerprint, requestDigest: checkpoint.requestDigest }));
  response = await pending;
  assert.equal(response.status, 502);
  assert.equal(imageCalls, 1);
  const events = readFileSync(fx.auditPath, 'utf8');
  assert.match(events, /generation_post_reserved.*image/);
  assert.match(events, /generation_http_received.*image/);
});

test('scope registration and image release waits are bounded and cost zero before forwarding', async (t) => {
  let calls = 0;
  const fx = await fixture(t, (_req, res) => {
    calls += 1;
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.end('data: [DONE]\n\n');
  }, { scopeWaitMs: 1_200, imageReviewWaitMs: 75 });
  const originalScope = readFileSync(fx.scopePath, 'utf8');
  rmSync(fx.scopePath);
  const pending = responseJson(`http://127.0.0.1:${fx.port}/v1/chat/completions`, makeLlm('source'));
  await new Promise((resolveWait) => setTimeout(resolveWait, 50));
  assert.equal(calls, 0);
  writeFileSync(fx.scopePath, originalScope);
  let response = await pending;
  assert.equal(response.status, 200, await response.text());
  assert.equal(calls, 1);

  activateImage(fx.dbPath);
  const image = { model: 'test-image', prompt: 'compiled prompt', n: 1, size: '1024x1536' };
  response = await responseJson(`http://127.0.0.1:${fx.port}/v1/images/generations`, image);
  assert.equal(response.status, 502);
  assert.equal(calls, 1);
  const events = readFileSync(fx.auditPath, 'utf8');
  assert.doesNotMatch(events, /generation_post_reserved.*image/);
});

test('an aborted downstream request still consumes its pre-forwarded source quota', async (t) => {
  let calls = 0;
  let received;
  const receivedPromise = new Promise((resolveReceived) => { received = resolveReceived; });
  const fx = await fixture(t, (_req, res) => {
    calls += 1;
    received();
    setTimeout(() => {
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.end('data: {"choices":[{"delta":{"content":"ok"}}]}\n\ndata: [DONE]\n\n');
    }, 150);
  });
  const controller = new AbortController();
  const aborted = fetch(`http://127.0.0.1:${fx.port}/v1/chat/completions`, {
    method: 'POST', headers: { authorization: 'Bearer test-private-token', 'content-type': 'application/json' },
    body: JSON.stringify(makeLlm('source')), signal: controller.signal,
  });
  await receivedPromise;
  controller.abort();
  await assert.rejects(aborted, { name: 'AbortError' });
  await new Promise((resolveWait) => setTimeout(resolveWait, 250));
  assert.equal(calls, 1);
  const second = await responseJson(`http://127.0.0.1:${fx.port}/v1/chat/completions`, makeLlm('source'));
  assert.equal(second.status, 502);
  assert.equal(calls, 1);
  const events = readFileSync(fx.auditPath, 'utf8');
  assert.match(events, /generation_post_reserved.*source/);
  assert.match(events, /downstream_aborted_after_forward.*source/);
});
