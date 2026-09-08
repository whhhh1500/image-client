import assert from 'node:assert/strict';
import { readFile, writeFile, stat } from 'node:fs/promises';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { settings, script, storyboard, pagePrompts } from './comic-md-fixtures.mjs';

const [cdpText, mockText, auditRoot, phase = 'initial'] = process.argv.slice(2);
const cdpPort = Number(cdpText), mockPort = Number(mockText);
assert(path.isAbsolute(auditRoot), 'Audit root must be absolute');
const report = { phase, checks: [], startedAt: new Date().toISOString() };
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const control = body => fetch(`http://127.0.0.1:${mockPort}/__control`, { method: 'POST', body: JSON.stringify(body) }).then(r => r.json());
const mockState = () => fetch(`http://127.0.0.1:${mockPort}/__state`).then(r => r.json());
async function eventually(fn, description, timeout = 45_000) {
  const deadline = Date.now() + timeout;
  let last;
  do { try { const result = await fn(); if (result) return result; } catch (error) { last = error; } await pause(200); } while (Date.now() < deadline);
  throw new Error(`Timed out: ${description}; ${last ?? ''}`);
}
const target = await eventually(async () => {
  const list = await fetch(`http://127.0.0.1:${cdpPort}/json/list`).then(r => r.json());
  return list.find(t => t.type === 'page' && t.webSocketDebuggerUrl);
}, 'isolated WebView CDP target');
assert(/localhost|tauri/.test(target.url), `Unexpected WebView URL ${target.url}`);
const socket = new WebSocket(target.webSocketDebuggerUrl);
const pending = new Map(); let nextId = 1;
await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true }); });
socket.addEventListener('message', event => {
  const message = JSON.parse(String(event.data));
  const entry = pending.get(message.id);
  if (!entry) return;
  pending.delete(message.id); clearTimeout(entry.timer);
  message.error ? entry.reject(new Error(message.error.message)) : entry.resolve(message.result);
});
function send(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = nextId++;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP ${method} timed out`)); }, 60_000);
    pending.set(id, { resolve, reject, timer }); socket.send(JSON.stringify({ id, method, params }));
  });
}
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true, userGesture: true });
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.exception?.value ?? result.exceptionDetails.text);
  return result.result.value;
}
const invoke = (command, args) => evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, ${JSON.stringify(args)})`);
const check = (name, evidence = true) => { report.checks.push({ name, evidence }); console.log(`PASS ${name}`); };
async function rejects(command, args, description) {
  let error;
  try { await invoke(command, args); } catch (e) { error = String(e); }
  assert(error, `${description}: command unexpectedly succeeded`);
  check(description, error);
  return error;
}
let scope;
const workspace = () => invoke('comic_md_workspace_get', { input: scope });
const doc = (ws, kind, pageNo) => ws.documents.find(d => d.kind === kind && (pageNo === undefined || d.pageNo === pageNo));
async function waitJob(job, expected = 'succeeded') {
  const final = await eventually(async () => {
    const ws = await workspace();
    const current = ws.jobs.find(j => j.id === job.id);
    return current && current.status !== 'running' ? { ws, job: current } : null;
  }, `job ${job.kind} ${job.id}`);
  assert.equal(final.job.status, expected, JSON.stringify(final.job));
  return final;
}
async function generate(stage, markdown) {
  await control({ markdown });
  const before = await workspace();
  const job = await invoke('comic_md_generate', { input: { ...scope, stage, expectedSourceRevisionId: before.sourceRevisionId } });
  const result = await waitJob(job);
  check(`pure Markdown ${stage} generation`, { job: result.job, documents: result.ws.documents.map(d => ({ kind: d.kind, pageNo: d.pageNo, issues: d.issues })) });
  return result.ws;
}
async function verifyOnlyMarkdownEntryAndRetainedTools() {
  const beforeCount = (await mockState()).requests.length;
  const clickButton = async text => evaluate(`(() => { const button = [...document.querySelectorAll('button')].find(b => b.textContent.trim() === ${JSON.stringify(text)}); if (!button) throw new Error('Missing navigation: ' + ${JSON.stringify(text)}); button.click(); })()`);
  await clickButton('通用短剧流水线');
  await eventually(() => evaluate(`document.body.innerText.includes('输入原文或打开历史文档')`), 'retained generic drama workflow renders');
  await evaluate(`([...document.querySelectorAll('button')].find(b => b.textContent.trim().startsWith('资产库'))).click()`);
  await eventually(() => evaluate(`Boolean(document.querySelector('input[placeholder="搜索标题、正文、模型或提示词"]'))`), 'retained asset library renders');
  await clickButton('图像生成');
  await eventually(() => evaluate(`Boolean(document.querySelector('textarea[placeholder="描述你想生成的画面…"]'))`), 'retained standalone image generator renders');
  await clickButton('视频');
  await eventually(() => evaluate(`Boolean(document.querySelector('textarea[placeholder="描述视频内容…"]'))`), 'retained video generator renders');
  await clickButton('图像');
  await clickButton('小说漫画');
  await eventually(() => evaluate(`Boolean(document.querySelector('[aria-label="小说漫画工作台"]'))`), 'sole Markdown comic workspace');
  const text = await evaluate(`document.querySelector('[aria-label="小说漫画工作台"]').innerText`);
  assert(!/旧版资料|小说分析与产物|历史漫画工作台|批准|14\s*项产物|十四类|全自动生产/.test(text), 'Retired novel-comic workflow is still visible');
  assert.equal((await mockState()).requests.length, beforeCount);
  check('sole Markdown comic entry removes legacy controls while generic drama image video assets remain usable', { retainedViews: ['通用短剧流水线', '资产库', '图像生成', '视频生成'], comicText: text });
  await clickButton('图像生成');
}
async function verifyRetiredIpcCommands() {
  const commands = JSON.parse(await readFile(new URL('./comic-md-retired-commands.json', import.meta.url), 'utf8'));
  assert.equal(commands.length, 87, 'Retirement manifest must enumerate all 87 formerly registered commands');
  assert.equal(new Set(commands).size, commands.length, 'Retirement manifest must not duplicate commands');
  const before = (await mockState()).requests.length;
  const errors = [];
  for (const command of commands) {
    let error;
    try { await invoke(command, { input: {} }); } catch (cause) { error = String(cause); }
    assert(error && /command\s+.+\s+not found/i.test(error), `${command} must be unregistered, not fail parameter validation: ${error}`);
    errors.push({ command, error });
  }
  assert.equal((await mockState()).requests.length, before);
  check('all retired novel comic IPC commands are unregistered without provider requests', { count: commands.length, errors });
  const supervisor = JSON.parse(await readFile(path.join(auditRoot, 'supervisor.json'), 'utf8'));
  const catalog = await fetch(`http://127.0.0.1:${supervisor.apiPort}/api/v1/catalog/tools`).then(r => { assert(r.ok); return r.json(); });
  const catalogText = JSON.stringify(catalog);
  assert(!/novel_|comic_|\/novel[\/\"]|\/comic[\/\"]/.test(catalogText));
  assert(catalogText.includes('/agents/') && catalogText.includes('/media/images/') && catalogText.includes('/media/videos/'));
  check('REST catalog exposes retained drama and media tools without retired novel comic tools');
}
async function verifyLegacyTasksRetiredAfterRestart() {
  const before = JSON.parse(await readFile(path.join(auditRoot, 'legacy-retirement-checkpoint.json'), 'utf8'));
  const db = new DatabaseSync(confined(path.join(auditRoot, 'data', 'image-client.db')), { readOnly: true });
  let pending, batch;
  try {
    pending = db.prepare('SELECT * FROM novel_production_jobs WHERE id=?').get(before.pending.id);
    batch = db.prepare('SELECT * FROM comic_visual_batches WHERE id=?').get(before.batch.id);
    assert(pending && batch, 'Old pending history must not be deleted');
    assert.equal(pending.status, 'error');
    assert.equal(batch.status, 'failed');
    assert.equal(pending.safe_error_code, 'LEGACY_COMIC_RETIRED');
    assert.equal(batch.safe_error_code, 'LEGACY_COMIC_RETIRED');
    assert.match(pending.safe_user_message, /旧小说漫画流程已停用/);
    for (const field of ['id', 'project_id', 'novel_work_id', 'novel_chapter_id', 'source_revision_id', 'request_hash', 'idempotency_key', 'created_at']) {
      assert.equal(pending[field], before.pending[field], `Retirement must preserve pending ${field}`);
      assert.equal(batch[field], before.batch[field], `Retirement must preserve batch ${field}`);
    }
    assert.equal(batch.authorization_json, before.batch.authorization_json);
    assert.equal(batch.authorization_fingerprint, before.batch.authorization_fingerprint);
    const completed = db.prepare('SELECT * FROM novel_production_jobs WHERE id=?').get(before.completed.id);
    assert.deepEqual(JSON.parse(JSON.stringify(completed)), before.completed, 'Finished historical job must be retained unchanged');
    assert.equal(db.prepare('SELECT content FROM novel_chapter_revisions WHERE id=?').get(before.pending.source_revision_id).content, before.sourceContent);
    assert.equal(db.prepare('SELECT COUNT(*) AS count FROM source_analysis_runs').get().count, before.sourceAnalysisCount);
    assert.equal(db.prepare('SELECT COUNT(*) AS count FROM adaptation_analysis_runs').get().count, before.adaptationAnalysisCount);
    assert.deepEqual(db.prepare('PRAGMA foreign_key_check').all(), []);
  } finally { db.close(); }
  assert.equal((await mockState()).requests.length, before.providerRequestCount, 'Application startup must not dispatch old authorized work');
  check('startup terminates legitimate old authorized recovery candidates without provider dispatch and preserves history', { matchedOldRecoveryCandidates: before.recoveryCandidates, job: { id: pending.id, status: pending.status, code: pending.safe_error_code }, batch: { id: batch.id, status: batch.status, code: batch.safe_error_code }, finishedHistoryPreserved: true, providerRequests: before.providerRequestCount });
}
async function verifyTruncatedTextModes() {
  for (const textMode of ['sse_length', 'nonstream_length', 'early_eof']) {
    const before = await workspace();
    const beforeCount = (await mockState()).requests.length;
    const markdown = `${settings}\n\n补充说明：这是节点齐全但传输未正常完成的 ${textMode} 候选。`;
    await control({ markdown, textMode });
    const job = await invoke('comic_md_generate', { input: { ...scope, stage: 'settings', expectedSourceRevisionId: before.sourceRevisionId } });
    const result = await waitJob(job, 'failed');
    assert.equal(result.job.outputMarkdown, markdown, `${textMode} must retain raw Markdown for manual repair`);
    assert.deepEqual(result.ws.documents, before.documents, `${textMode} must not overwrite valid documents`);
    await pause(500);
    assert.equal((await mockState()).requests.length, beforeCount + 1, `${textMode} must not automatically retry`);
    check(`${textMode} is failed despite complete MD nodes, preserves raw and current documents, without retry`, result.job);
  }
}
function confined(raw) {
  const absolute = path.resolve(raw), relative = path.relative(auditRoot, absolute);
  assert(relative && !relative.startsWith('..') && !path.isAbsolute(relative), `Output escaped isolated root: ${raw}`);
  return absolute;
}
async function verifyChapterRenameUi(otherChapterId) {
  const desiredTitle = '夜雨来信 · 标题修改已保存';
  await evaluate(`(() => {
    localStorage.setItem(${JSON.stringify(`comic-md:work:${scope.projectId}`)}, JSON.stringify(${JSON.stringify(scope.novelWorkId)}));
    localStorage.setItem(${JSON.stringify(`comic-md:chapter:${scope.projectId}:${scope.novelWorkId}`)}, JSON.stringify(${JSON.stringify(scope.chapterId)}));
    [...document.querySelectorAll('button')].find(b => b.textContent.trim() === '小说漫画').click();
  })()`);
  await eventually(() => evaluate(`Boolean(document.querySelector('textarea[aria-label="小说原文预览"]'))`), 'read-only novel source preview');
  await evaluate(`(() => {
    const button = [...document.querySelectorAll('section[aria-label="小说原文资产"] button')].find(b => b.textContent.trim() === '管理小说原文');
    if (!button) throw new Error('Missing shared source manager entry');
    button.click();
  })()`);
  await eventually(() => evaluate(`Boolean(document.querySelector('[role="dialog"]'))`), 'shared source manager opens');
  await evaluate(`([...document.querySelectorAll('button')].find(b => b.textContent.trim() === '编辑并保存新修订')).click()`);
  await eventually(() => evaluate(`Boolean(document.querySelector('input[aria-label="章节名称"]'))`), 'shared manager chapter title input');
  await evaluate(`(() => {
    const input = document.querySelector('input[aria-label="章节名称"]');
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, ${JSON.stringify(desiredTitle)});
    input.dispatchEvent(new Event('input', {bubbles:true}));
  })()`);
  await eventually(() => evaluate(`document.querySelector('input[aria-label="章节名称"]')?.value === ${JSON.stringify(desiredTitle)}`), 'typed title in shared manager form');
  await evaluate(`([...document.querySelectorAll('button')].find(b => b.textContent.trim() === '保存新修订')).click()`);
  await eventually(() => evaluate(`!document.querySelector('input[aria-label="章节名称"]') && document.querySelector('textarea[aria-label="小说原文预览"]')?.value`), 'source manager save returns to comic preview');
  const dropdownTitle = await evaluate(`document.querySelector('select[aria-label="选择章节"]')?.selectedOptions[0]?.textContent`);
  const readOnlyDb = new DatabaseSync(confined(path.join(auditRoot, 'data', 'image-client.db')), { readOnly: true });
  let databaseTitle;
  try { databaseTitle = readOnlyDb.prepare('SELECT title FROM novel_chapters WHERE id = ?').get(scope.chapterId)?.title; }
  finally { readOnlyDb.close(); }
  await evaluate(`(() => { const select = document.querySelector('select[aria-label="选择章节"]'); Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(select, ${JSON.stringify(otherChapterId)}); select.dispatchEvent(new Event('change', {bubbles:true})); })()`);
  await eventually(() => evaluate(`document.querySelector('select[aria-label="选择章节"]')?.value === ${JSON.stringify(otherChapterId)}`), 'switch to other chapter');
  await evaluate(`(() => { const select = document.querySelector('select[aria-label="选择章节"]'); Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(select, ${JSON.stringify(scope.chapterId)}); select.dispatchEvent(new Event('change', {bubbles:true})); })()`);
  await eventually(() => evaluate(`document.querySelector('select[aria-label="选择章节"]')?.value === ${JSON.stringify(scope.chapterId)} && Boolean(document.querySelector('textarea[aria-label="小说原文预览"]'))`), 'switch back to renamed chapter');
  const returnedSource = await evaluate(`document.querySelector('textarea[aria-label="小说原文预览"]')?.value`);
  const observation = { desiredTitle, dropdownTitle, databaseTitle, returnedSource };
  report.chapterRenameObservation = observation;
  assert.equal(databaseTitle, desiredTitle, JSON.stringify(observation));
  assert(dropdownTitle?.includes(desiredTitle), JSON.stringify(observation));
  assert.equal(returnedSource, (await workspace()).sourceContent, JSON.stringify(observation));
  check('shared source manager saves chapter metadata, refreshes dropdown and preserves the read-only comic source through chapter switching', observation);
}
function readRevisionSnapshot() {
  const db = new DatabaseSync(confined(path.join(auditRoot, 'data', 'image-client.db')), { readOnly: true });
  try {
    return {
      chapter: db.prepare('SELECT title, current_revision_id FROM novel_chapters WHERE id = ?').get(scope.chapterId),
      sourceRevisionCount: db.prepare('SELECT COUNT(*) AS count FROM novel_chapter_revisions WHERE novel_chapter_id = ?').get(scope.chapterId).count,
      documents: db.prepare('SELECT id, revision, markdown, dependencies FROM comic_md_documents WHERE novel_work_id = ? ORDER BY id').all(scope.novelWorkId),
    };
  } finally { db.close(); }
}
async function verifyDisabledSave(label) {
  const before = await workspace();
  const beforeDatabase = readRevisionSnapshot();
  const beforeRequests = (await mockState()).requests.length;
  const disabled = await evaluate(`(async () => {
      const button = [...document.querySelectorAll('button')].find(b => b.textContent.trim() === ${JSON.stringify(label)});
      if (!button) throw new Error('Missing unchanged save button');
      button.click(); await new Promise(resolve => setTimeout(resolve, 150)); return button.disabled;
    })()`);
  assert.equal(disabled, true, `${label} must be disabled when unchanged`);
  const after = await workspace();
  assert.equal(after.sourceRevisionId, before.sourceRevisionId);
  assert.deepEqual(after.documents, before.documents);
  assert.deepEqual(readRevisionSnapshot(), beforeDatabase);
  assert.equal((await mockState()).requests.length, beforeRequests);
  check(`${label} unchanged state is disabled and clicking preserves workspace DB revisions and provider count`, { disabled, sourceRevisionId: before.sourceRevisionId, sourceRevisionCount: beforeDatabase.sourceRevisionCount, documentCount: before.documents.length, providerRequests: beforeRequests });
}
async function inspectCopyUi(expectedMarkdown) {
  await evaluate(`(() => {
    localStorage.setItem(${JSON.stringify('comic-md:work:')} + ${JSON.stringify(scope.projectId)}, JSON.stringify(${JSON.stringify(scope.novelWorkId)}));
    localStorage.setItem(${JSON.stringify(`comic-md:chapter:${scope.projectId}:${scope.novelWorkId}`)}, JSON.stringify(${JSON.stringify(scope.chapterId)}));
    const button = [...document.querySelectorAll('button')].find(b => b.textContent.trim() === '小说漫画');
    if (!button) throw new Error('Missing 小说漫画 entry'); button.click();
  })()`);
  await eventually(() => evaluate(`Boolean(document.querySelector('nav[aria-label="制作步骤"]'))`), 'Markdown production navigation');
  await evaluate(`(() => { const button = [...document.querySelectorAll('nav[aria-label="制作步骤"] button')].find(b => b.textContent.includes('Prompt')); if (!button) throw new Error('No Prompt step'); button.click(); })()`);
  await eventually(() => evaluate(`document.querySelector('textarea[aria-label="本页 Prompt Markdown"]')?.value === ${JSON.stringify(expectedMarkdown)}`), 'saved page Prompt visible in editor');
  await verifyDisabledSave('保存文字');
  const copied = await evaluate(`(async () => {
    const clipboard = navigator.clipboard;
    const previous = Object.getOwnPropertyDescriptor(clipboard, 'writeText');
    let captured;
    Object.defineProperty(clipboard, 'writeText', { configurable: true, value: async value => { captured = value; } });
    try {
      const button = [...document.querySelectorAll('button')].find(b => b.textContent.trim() === '复制全文');
      if (!button) throw new Error('Missing copy button'); button.click();
      await new Promise(resolve => setTimeout(resolve, 100)); return captured;
    } finally { if (previous) Object.defineProperty(clipboard, 'writeText', previous); else delete clipboard.writeText; }
  })()`);
  assert.equal(copied, expectedMarkdown);
  check('real UI copy button supplies full Markdown to clipboard API', { length: copied.length, clipboardBoundaryIntercepted: true });
  if (phase === 'restore') {
    await evaluate(`([...document.querySelectorAll('nav[aria-label="制作步骤"] button')].find(b => b.textContent.includes('漫画'))).click()`);
    const displayedImage = await eventually(() => evaluate(`(() => { const img = document.querySelector('img[alt="第1页漫画"]'); return img?.complete && img.naturalWidth > 0 ? { src: img.src, naturalWidth: img.naturalWidth, naturalHeight: img.naturalHeight } : null; })()`), 'asset protocol displays actual returned image');
    await evaluate(`([...document.querySelectorAll('button')].find(b => b.textContent.trim() === '查看原图')).click()`);
    await eventually(() => evaluate(`(() => { const dialog = document.querySelector('[role="dialog"][aria-label="第1页原图"]'); const img = dialog?.querySelector('img'); return Boolean(dialog && img?.complete && img.naturalWidth > 0); })()`), 'in-app full image dialog');
    await evaluate(`([...document.querySelectorAll('button')].find(b => b.textContent.trim() === '关闭大图')).click()`);
    assert.equal(await evaluate(`Boolean(document.querySelector('[role="dialog"][aria-label="第1页原图"]'))`), false);
    check('actual image loads through asset protocol and in-app full image dialog opens and closes', displayedImage);
    await evaluate(`([...document.querySelectorAll('nav[aria-label="制作步骤"] button')].find(b => b.textContent.includes('Prompt'))).click()`);
    await eventually(() => evaluate(`Boolean(document.querySelector('textarea[aria-label="本页 Prompt Markdown"]'))`), 'return to Prompt editor');
  }
  await send('Emulation.setDeviceMetricsOverride', { width: 960, height: 640, deviceScaleFactor: 1, mobile: false });
  await pause(150);
  await evaluate(`document.querySelector('section[aria-label="本页 Prompt编辑器"]')?.scrollIntoView({block:'start'})`);
  await evaluate(`([...document.querySelectorAll('button')].find(b => b.textContent.trim() === '生成这一页漫画'))?.scrollIntoView({block:'center'})`);
  const smallLayout = await evaluate(`(() => {
    const button = [...document.querySelectorAll('button')].find(b => b.textContent.trim() === '生成这一页漫画');
    const rect = button?.getBoundingClientRect();
    const main = document.querySelector('nav[aria-label="制作步骤"]')?.nextElementSibling;
    return { width: innerWidth, height: innerHeight, mainHeight: main?.getBoundingClientRect().height ?? 0, pageOverflow: document.documentElement.scrollWidth > innerWidth + 1, mainOverflow: main ? main.scrollWidth > main.clientWidth + 1 : true, editorPresent: Boolean(document.querySelector('textarea[aria-label="本页 Prompt Markdown"]')), renderButtonReachable: Boolean(rect && rect.width > 0 && rect.height > 0 && rect.left >= 0 && rect.right <= innerWidth && rect.top >= 0 && rect.bottom <= innerHeight) };
  })()`);
  assert.equal(smallLayout.pageOverflow, false);
  assert.equal(smallLayout.mainOverflow, false);
  assert.equal(smallLayout.editorPresent, true);
  assert.equal(smallLayout.renderButtonReachable, true);
  assert(smallLayout.mainHeight >= 260, `Minimum viewport main height below 260px: ${smallLayout.mainHeight}`);
  smallLayout.meetsPreferred280px = smallLayout.mainHeight >= 280;
  check('minimum 960x640 viewport has reachable page action and no horizontal overflow', smallLayout);
  const smallScreenshot = await send('Page.captureScreenshot', { format: 'png' });
  await writeFile(path.join(auditRoot, `markdown-ui-${phase}-960x640.png`), Buffer.from(smallScreenshot.data, 'base64'));
  await send('Emulation.clearDeviceMetricsOverride');
  await pause(150);
  const screenshot = await send('Page.captureScreenshot', { format: 'png' });
  await writeFile(path.join(auditRoot, `markdown-ui-${phase}.png`), Buffer.from(screenshot.data, 'base64'));
}
try {
  await eventually(() => evaluate(`Boolean(window.__TAURI_INTERNALS__?.invoke && document.querySelector('#root')?.children.length)`), 'frontend hydration');
  await pause(1200);
  if (phase === 'restore') {
    const saved = JSON.parse(await readFile(path.join(auditRoot, 'checkpoint.json'), 'utf8'));
    scope = saved.scope;
    await verifyLegacyTasksRetiredAfterRestart();
    const ws = await workspace();
    assert.deepEqual(ws.documents, saved.documents);
    assert.equal(ws.images.length, saved.images.length);
    const interrupted = ws.jobs.find(j => j.id === saved.runningJobId);
    assert.equal(interrupted?.status, 'interrupted');
    const count = (await mockState()).requests.length;
    await pause(1500);
    assert.equal((await mockState()).requests.length, count);
    check('restart preserves exact Markdown revisions and images', { documents: ws.documents.length, images: ws.images.length });
    check('restart marks running task interrupted without provider resubmission', interrupted);
    await inspectCopyUi(doc(ws, 'page_prompt', 1).markdown);
  } else {
    const base = `http://127.0.0.1:${mockPort}/v1`;
    await invoke('save_config', { config: { imageApiUrl: `${base}/images/generations`, imageApiKey: 'isolated-fixture', imageApiModel: 'gpt-image-2', llmApiUrl: base, llmApiKey: 'isolated-fixture', llmApiModel: 'gemini-3.7-flash', videoApiUrl: '', videoApiKey: '', videoApiModel: 'kling-video-v3', outputDir: path.join(auditRoot, 'data', 'assets') } });
    const active = await invoke('db_select', { query: 'SELECT value FROM settings WHERE key = ?', bindValues: ['active_project'] });
    const projectId = active[0]?.value;
    assert(projectId, 'Normal frontend must initialize an active project');
    await verifyOnlyMarkdownEntryAndRetainedTools();
    await verifyRetiredIpcCommands();
    const work = await invoke('novel_work_create', { input: { projectId, title: '夜雨来信', idempotencyKey: 'md-audit-work' } });
    const sourceInput = { projectId, novelWorkId: work.id, chapterNo: 1, title: '夜雨', content: '林青左臂负伤，右手带着未拆封的信走入客栈后院。他望着木门低声说：别出声。', idempotencyKey: 'md-audit-source-1' };
    let source = await invoke('novel_chapter_revision_create', { input: sourceInput });
    scope = { projectId, novelWorkId: work.id, chapterId: source.chapterId };
    let ws = await workspace();
    assert.equal(ws.sourceRevisionId, source.id);
    assert.equal(ws.sourceContent, sourceInput.content);
    assert.equal((await mockState()).requests.length, 0);
    check('create novel and save source without provider requests', scope);
    const otherChapter = await invoke('novel_chapter_revision_create', { input: { ...sourceInput, chapterNo: 99, title: '切换验证章', content: '用于验证切章后标题保存。', idempotencyKey: 'md-audit-rename-other' } });
    await verifyChapterRenameUi(otherChapter.chapterId);
    ws = await workspace();
    source = { ...source, id: ws.sourceRevisionId };
    assert.equal((await mockState()).requests.length, 0);
    const incomplete = await invoke('comic_md_document_save', { input: { ...scope, kind: 'settings', markdown: '# 草稿\n## 世界观\n夜雨客栈。', expectedRevision: null } });
    assert(incomplete.issues.length > 0);
    const missingNodesError = await rejects('comic_md_generate', { input: { ...scope, stage: 'script', expectedSourceRevisionId: (await workspace()).sourceRevisionId } }, 'missing necessary MD nodes block downstream generation');
    assert.match(missingNodesError, /请先保存完整且最新的作品设定/, 'Must fail specifically on settings readiness, not source revision or configuration');
    assert.equal((await mockState()).requests.length, 0);
    check('incomplete Markdown draft remains saved and editable', incomplete);
    for (const [stage, markdown] of [['settings', settings], ['script', script], ['storyboard', storyboard], ['page_prompts', pagePrompts]]) ws = await generate(stage, markdown);
    assert.equal(ws.documents.length, 5);
    assert.equal(ws.documents.filter(d => d.kind === 'page_prompt').length, 2);
    assert(ws.documents.every(d => !d.stale && d.issues.length === 0));
    assert.equal(ws.imageReady, true);
    const textRequests = (await mockState()).requests.filter(r => r.kind === 'text');
    assert.equal(textRequests.length, 4);
    assert(textRequests.every(r => !['json_schema', 'json_object'].includes(r.body.response_format?.type)));
    check('all five Markdown documents ready without JSON artifacts');
    await verifyTruncatedTextModes();

    const page1 = doc(ws, 'page_prompt', 1);
    const saved = await invoke('comic_md_document_save', { input: { ...scope, kind: 'page_prompt', pageNo: 1, markdown: page1.markdown.replace('## 画面要求\n', '## 画面要求\n油灯保持暖色。\n'), expectedRevision: page1.revision } });
    assert(saved.revision > page1.revision);
    const history = await invoke('comic_md_document_history', { input: { ...scope, documentId: page1.id } });
    assert(history.some(h => h.revision === page1.revision && h.markdown === page1.markdown));
    check('manual edit preserves prior Markdown revision', history.map(h => h.revision));
    await rejects('comic_md_document_save', { input: { ...scope, kind: 'page_prompt', pageNo: 1, markdown: page1.markdown, expectedRevision: page1.revision } }, 'optimistic revision conflict rejects lost update');
    await rejects('comic_md_document_history', { input: { ...scope, projectId: 'foreign-project', documentId: page1.id } }, 'cross project history access rejected');
    const second = await invoke('novel_chapter_revision_create', { input: { ...sourceInput, chapterNo: 2, content: '次日清晨，林青离开客栈。', idempotencyKey: 'md-audit-source-2' } });
    const secondScope = { ...scope, chapterId: second.chapterId };
    const secondWs = await invoke('comic_md_workspace_get', { input: secondScope });
    assert(secondWs.documents.every(d => d.kind === 'settings'));
    await rejects('comic_md_document_history', { input: { ...secondScope, documentId: page1.id } }, 'cross chapter page history access rejected');
    check('chapter isolation shares only work settings');
    const secondScript = await invoke('comic_md_document_save', { input: { ...secondScope, kind: 'script', markdown: script, expectedRevision: null } });
    assert.equal(secondScript.stale, false);
    const secondSourceUpdated = await invoke('novel_chapter_revision_create', { input: { ...sourceInput, chapterId: second.chapterId, chapterNo: 2, content: '次日清晨，林青留在客栈等待回信。', idempotencyKey: 'md-audit-source-2-update' } });
    const changedSecond = await invoke('comic_md_workspace_get', { input: secondScope });
    assert.equal(changedSecond.sourceRevisionId, secondSourceUpdated.id);
    assert.equal(doc(changedSecond, 'script').stale, true);
    assert.equal(doc(changedSecond, 'script').markdown, script);
    assert.equal(doc(changedSecond, 'settings').stale, false);
    assert((await workspace()).documents.every(d => !d.stale), 'Future chapter revision must not stale earlier chapter');
    check('source revision change preserves but stales chapter document without contaminating earlier chapter');

    ws = await workspace();
    const exported = await invoke('comic_md_export', { input: scope });
    confined(exported.path);
    assert.equal(exported.files.length, ws.documents.length);
    const contents = [];
    for (const file of exported.files) contents.push(await readFile(confined(path.isAbsolute(file) ? file : path.join(exported.path, file)), 'utf8'));
    for (const document of ws.documents) assert(contents.some(content => content === document.markdown), `Export altered ${document.kind}/${document.pageNo}`);
    check('MD export writes exact complete document contents to real isolated files', exported);
    await inspectCopyUi(doc(ws, 'page_prompt', 1).markdown);

    const pages = ws.documents.filter(d => d.kind === 'page_prompt').sort((a,b) => a.pageNo - b.pageNo);
    let rendered = await waitJob(await invoke('comic_md_render', { input: { ...scope, pages: [{ documentId: pages[0].id, revision: pages[0].revision }] } }));
    assert.equal(rendered.ws.images.length, 1);
    assert((await stat(confined(rendered.ws.images[0].path))).size > 0);
    const request = (await mockState()).requests.filter(r => r.kind === 'image')[0];
    assert.equal(request.body.prompt, pages[0].markdown);
    check('single page image uses exact saved current Markdown and persists actual image', rendered.ws.images[0]);
    const imageCount = (await mockState()).imageCalls;
    await control({ failImageAt: imageCount + 2 });
    rendered = await waitJob(await invoke('comic_md_render', { input: { ...scope, pages: pages.map(d => ({ documentId: d.id, revision: d.revision })) } }), 'failed');
    assert.equal(rendered.job.completedPages, 1);
    assert.equal(rendered.ws.images.length, 2);
    check('batch failure stops and retains completed page plus previous image', { job: rendered.job, images: rendered.ws.images.length });

    const originalScope = scope;
    const standaloneWork = await invoke('novel_work_create', { input: { projectId, title: '独立提示词使用', idempotencyKey: 'md-audit-standalone-work' } });
    const standaloneSource = await invoke('novel_chapter_revision_create', { input: { projectId, novelWorkId: standaloneWork.id, chapterNo: 1, title: '独立页', content: '使用用户自行写好的完整页提示词。', idempotencyKey: 'md-audit-standalone-source' } });
    scope = { projectId, novelWorkId: standaloneWork.id, chapterId: standaloneSource.chapterId };
    const standalonePage = await invoke('comic_md_document_save', { input: { ...scope, kind: 'page_prompt', pageNo: 1, markdown: pages[0].markdown, expectedRevision: null } });
    const standaloneWorkspace = await workspace();
    assert.equal(standaloneWorkspace.documents.length, 1);
    assert.equal(standaloneWorkspace.imageReady, true);
    const standaloneRendered = await waitJob(await invoke('comic_md_render', { input: { ...scope, pages: [{ documentId: standalonePage.id, revision: standalonePage.revision }] } }));
    assert.equal(standaloneRendered.ws.images.length, 1);
    assert((await stat(confined(standaloneRendered.ws.images[0].path))).size > 0);
    assert.equal((await mockState()).requests.filter(r => r.kind === 'image').at(-1).body.prompt, standalonePage.markdown);
    check('handwritten standalone page renders without settings script or storyboard documents', { documents: standaloneWorkspace.documents.length, image: standaloneRendered.ws.images[0] });
    scope = originalScope;

    const settingsDoc = doc(rendered.ws, 'settings');
    await invoke('comic_md_document_save', { input: { ...scope, kind: 'settings', markdown: `${settingsDoc.markdown}\n\n补充：城门每天日落关闭。`, expectedRevision: settingsDoc.revision } });
    ws = await workspace();
    assert(ws.documents.filter(d => d.kind !== 'settings').every(d => d.stale));
    assert(ws.images.every(i => i.stale));
    const countBeforeBlocked = (await mockState()).requests.length;
    await rejects('comic_md_render', { input: { ...scope, pages: [{ documentId: pages[0].id, revision: pages[0].revision }] } }, 'stale page cannot request image');
    assert.equal((await mockState()).requests.length, countBeforeBlocked);
    check('dependency edit preserves documents and images while marking stale');
    await eventually(() => evaluate(`(() => { const button = [...document.querySelectorAll('button')].find(b => b.textContent.trim() === '保存更新'); return button && !button.disabled; })()`), 'stale unchanged document can still be explicitly saved after review');
    check('stale unchanged Markdown still offers enabled save update action');
    for (const [stage, markdown] of [['script', script], ['storyboard', storyboard], ['page_prompts', pagePrompts]]) ws = await generate(stage, markdown);

    const beforeHeldImageCount = (await mockState()).imageCalls;
    await control({ failImageAt: null, holdImages: true });
    const restartPage = doc(ws, 'page_prompt', 1);
    const running = await invoke('comic_md_render', { input: { ...scope, pages: [{ documentId: restartPage.id, revision: restartPage.revision }] } });
    await eventually(async () => (await mockState()).imageCalls > beforeHeldImageCount, 'held request reached local mock');
    await rejects('comic_md_render', { input: { ...scope, pages: [{ documentId: restartPage.id, revision: restartPage.revision }] } }, 'running persisted task prevents duplicate render submission');
    assert.equal((await mockState()).imageCalls, beforeHeldImageCount + 1);
    ws = await workspace();
    assert.equal(ws.jobs.find(j => j.id === running.id)?.status, 'running');
    await writeFile(path.join(auditRoot, 'checkpoint.json'), JSON.stringify({ scope, documents: ws.documents, images: ws.images, runningJobId: running.id }, null, 2));
    check('persisted running image task prepared for actual process restart', running.id);
  }
  assert(!(await mockState()).requests.some(r => r.kind === 'unexpected'), 'Unexpected provider endpoint');
  report.status = 'passed';
} catch (error) {
  report.status = 'failed'; report.error = String(error.stack ?? error); process.exitCode = 1;
  console.error(report.error);
  try {
    report.visibleText = await evaluate(`document.body.innerText`);
    const screenshot = await send('Page.captureScreenshot', { format: 'png' });
    await writeFile(path.join(auditRoot, `failure-${phase}.png`), Buffer.from(screenshot.data, 'base64'));
  } catch (captureError) { report.captureError = String(captureError); }
} finally {
  report.finishedAt = new Date().toISOString();
  await writeFile(path.join(auditRoot, `acceptance-${phase}.json`), JSON.stringify(report, null, 2));
  socket.close();
}
