import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { settings, script, pagePrompt } from './comic-md-fixtures.mjs';

const [cdpText, mockText, auditRoot, phase = 'initial'] = process.argv.slice(2);
assert(path.isAbsolute(auditRoot));
const report = { phase, scenario: 'ai', checks: [], startedAt: new Date().toISOString() };
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const mockState = () => fetch(`http://127.0.0.1:${mockText}/__state`).then(r => r.json());
const control = body => fetch(`http://127.0.0.1:${mockText}/__control`, { method: 'POST', body: JSON.stringify(body) }).then(r => r.json());
async function until(fn, description, timeout = 45000) {
  let last; const deadline = Date.now() + timeout;
  do { try { const value = await fn(); if (value) return value; } catch (error) { last = error; } await pause(150); } while (Date.now() < deadline);
  throw new Error(`Timed out: ${description}; ${last ?? ''}`);
}
const target = await until(async () => (await fetch(`http://127.0.0.1:${cdpText}/json/list`).then(r => r.json())).find(t => t.type === 'page' && t.webSocketDebuggerUrl), 'isolated WebView');
const socket = new WebSocket(target.webSocketDebuggerUrl), pending = new Map(); let seq = 0;
await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true }); });
socket.addEventListener('message', event => {
  const message = JSON.parse(String(event.data)), call = pending.get(message.id);
  if (!call) return;
  pending.delete(message.id); clearTimeout(call.timer);
  message.error ? call.reject(new Error(message.error.message)) : call.resolve(message.result);
});
function send(method, params = {}) {
  return new Promise((resolve, reject) => { const id = ++seq; const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP ${method} timeout`)); }, 60000); pending.set(id, { resolve, reject, timer }); socket.send(JSON.stringify({ id, method, params })); });
}
async function evaluate(expression) {
  const value = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true, userGesture: true });
  if (value.exceptionDetails) throw new Error(value.exceptionDetails.exception?.description ?? value.exceptionDetails.exception?.value ?? value.exceptionDetails.text);
  return value.result.value;
}
const invoke = (command, args) => evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);
const check = (name, evidence = true) => { report.checks.push({ name, evidence }); console.log(`PASS ${name}`); };
const labels = { instruction: 'AI 优化要求', versions: '文档版本', all: '优化全部页', injection: '本章 Prompt 注入', saveRules: '保存注入', optimize: 'AI 优化', prior: '上一版', next: '下一版', current: '返回当前草稿', adopt: '采用此版本并保存' };
let scope;
const workspace = () => invoke('comic_md_workspace_get', { input: scope });
const getDoc = (ws, kind, pageNo) => ws.documents.find(d => d.kind === kind && (pageNo === undefined || d.pageNo === pageNo));
const textRequests = async () => (await mockState()).requests.filter(r => r.kind === 'text');
const requestText = request => request.body.messages.map(message => typeof message.content === 'string' ? message.content : JSON.stringify(message.content)).join('\n');
const imageRequests = async () => (await mockState()).requests.filter(r => r.kind === 'image');
const history = document => invoke('comic_md_document_history', { input: { ...scope, documentId: document.id } });
const storyboard = Array.from({ length: 4 }, (_, i) => `# 第${i+1}页\n## 本页剧情\n林青${i+1}次观察木门。\n## 分镜\n### 第1格\n林青右手持信看向木门。\n## 画面文字\n无对白。\n## 人物状态\n左臂包扎，右手持信。`).join('\n\n');
const pageText = n => `${pagePrompt(n)}\n\n## 验收标记\n独立输入身份：PAGE_TOKEN_${n}`;
async function save(kind, markdown, instruction = '', pageNo) {
  const ws = await workspace(), previous = getDoc(ws, kind, pageNo);
  return invoke('comic_md_document_save', { input: { ...scope, kind, pageNo, markdown, optimizationInstruction: instruction, expectedRevision: previous?.revision ?? null } });
}
async function settled(job, expected = 'succeeded') {
  const result = await until(async () => { const ws = await workspace(), current = ws.jobs.find(j => j.id === job.id); return current && current.status !== 'running' ? { ws, job: current } : null; }, `job ${job.id}`);
  assert.equal(result.job.status, expected, JSON.stringify(result.job)); return result;
}
async function afterUiJob(kind, beforeIds, expected = 'succeeded') {
  const job = await until(async () => (await workspace()).jobs.find(j => j.kind === kind && !beforeIds.has(j.id)), `UI ${kind} creates durable job`);
  return settled(job, expected);
}
async function click(text) {
  await until(() => evaluate(`(() => { const button=[...document.querySelectorAll('button')].find(b=>b.textContent.trim()===${JSON.stringify(text)}); return Boolean(button && !button.disabled); })()`), `enabled button ${text}`);
  await evaluate(`(() => { const button=[...document.querySelectorAll('button')].find(b=>b.textContent.trim()===${JSON.stringify(text)}); if(!button)throw new Error('Missing button '+${JSON.stringify(text)}); if(button.disabled)throw new Error('Disabled button '+${JSON.stringify(text)}); button.click(); })()`);
}
async function setField(label, value, tag = 'textarea') {
  const selector = `${tag}[aria-label=${JSON.stringify(label)}]`;
  await until(() => evaluate(`Boolean(document.querySelector(${JSON.stringify(selector)}))`), `field ${label}`);
  await evaluate(`(() => { const input=document.querySelector(${JSON.stringify(selector)}); Object.getOwnPropertyDescriptor(${tag === 'select' ? 'HTMLSelectElement' : tag === 'input' ? 'HTMLInputElement' : 'HTMLTextAreaElement'}.prototype,'value').set.call(input,${JSON.stringify(value)}); input.dispatchEvent(new Event(${JSON.stringify(tag === 'select' ? 'change' : 'input')},{bubbles:true})); })()`);
  await until(() => evaluate(`document.querySelector(${JSON.stringify(selector)})?.value===${JSON.stringify(value)}`), `React accepts ${label}`);
}
const fieldValue = label => evaluate(`document.querySelector('[aria-label="'+${JSON.stringify(label)}+'"]')?.value`);
async function nav(view) {
  await evaluate(`(() => { const button=[...document.querySelectorAll('nav[aria-label="制作步骤"] button')].find(b=>b.textContent.includes(${JSON.stringify(view)})); if(!button)throw new Error('No step '+${JSON.stringify(view)}); button.click(); })()`);
  await evaluate(`document.querySelector('nav[aria-label="制作步骤"]')?.nextElementSibling?.scrollTo(0,0)`);
}
async function openScope(next) {
  scope = next;
  await evaluate(`(() => { const button=[...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='图像生成'); if(button)button.click(); localStorage.setItem(${JSON.stringify(`comic-md:work:${scope.projectId}`)},JSON.stringify(${JSON.stringify(scope.novelWorkId)})); localStorage.setItem(${JSON.stringify(`comic-md:chapter:${scope.projectId}:${scope.novelWorkId}`)},JSON.stringify(${JSON.stringify(scope.chapterId)})); })()`);
  await pause(200); await click('小说漫画');
  await until(() => evaluate(`Boolean(document.querySelector('nav[aria-label="制作步骤"]'))`), 'chapter workspace');
}
async function makeScope(projectId, title, key) {
  const work = await invoke('novel_work_create', { input: { projectId, title, idempotencyKey: `${key}-work` } });
  const source = await invoke('novel_chapter_revision_create', { input: { projectId, novelWorkId: work.id, chapterNo: 1, title: '夜雨', content: '林青负伤带信进入雨夜客栈，低声说别出声。', idempotencyKey: `${key}-source` } });
  return { projectId, novelWorkId: work.id, chapterId: source.chapterId };
}
async function optimizeDoc(document, instruction, output, extras = {}) {
  await control({ markdown: output, ...extras });
  return invoke('comic_md_optimize', { input: { ...scope, targets: [{ documentId: document.id, revision: document.revision }], instruction } });
}
async function capture(name) {
  const screenshot = await send('Page.captureScreenshot', { format: 'png' });
  await writeFile(path.join(auditRoot, name), Buffer.from(screenshot.data, 'base64'));
}
function assertActualImagePrompt(request, document, injection) {
  const suffix = injection.trim() ? '\n\n---\n\n## 本次漫画生成的优先规则\n以下是用户为本次漫画生成保存的补充要求。若与上方页 Prompt 的绘图要求冲突，以本节为准；未涉及的剧情、人物、分镜和画面文字仍按上方页 Prompt 执行。\n\n' + injection : '';
  assert.equal(request.body.prompt, document.markdown + suffix, 'Actual image prompt must preserve exact Markdown and append the complete frozen injection priority contract');
}
async function uiRender(text, expectedPages) {
  const before = await workspace(), ids = new Set(before.jobs.map(j => j.id)), count = (await imageRequests()).length;
  await click(`${text}（${expectedPages.length} 页）`); const result = await afterUiJob('images', ids);
  const requests = (await imageRequests()).slice(count);
  assert.equal(requests.length, expectedPages.length, `${text} request count`);
  expectedPages.forEach((n, i) => { const document = getDoc(before, 'page_prompt', n); assert(document); assertActualImagePrompt(requests[i], document, before.renderOptions.promptInjection); });
  check(`${text} sends exactly pages ${expectedPages.join(',')} with saved injection`, { pages: expectedPages, completedPages: result.job.completedPages, injection: before.renderOptions.promptInjection });
  return result.ws;
}

try {
  await until(() => evaluate(`Boolean(window.__TAURI_INTERNALS__?.invoke && document.querySelector('#root')?.children.length)`), 'frontend ready');
  await pause(1000);
  if (phase === 'restore') {
    const saved = JSON.parse(await readFile(path.join(auditRoot, 'ai-checkpoint.json'), 'utf8')); scope = saved.scope;
    const ws = await workspace();
    assert.deepEqual(ws.documents, saved.documents); assert.deepEqual(ws.renderOptions, saved.renderOptions); assert.deepEqual(ws.images, saved.images);
    assert.equal((await mockState()).requests.length, saved.providerCount);
    for (const document of ws.documents) { const versions = await history(document); assert(versions.some(v => v.revision === document.revision && v.optimizationInstruction === document.optimizationInstruction)); }
    check('restart preserves Markdown optimization instructions histories injection and image provenance without resubmission');
    await openScope(scope); await nav('漫画');
    await until(async () => await fieldValue(labels.injection) === ws.renderOptions.promptInjection, 'saved injection shown after restart');
    await capture('ai-comic-rules-restored.png');
    check('saved image injection is restored in the actual UI');
  } else {
    const base = `http://127.0.0.1:${mockText}/v1`;
    await invoke('save_config', { config: { imageApiUrl: `${base}/images/generations`, imageApiKey: 'isolated-fixture', imageApiModel: 'gpt-image-2', llmApiUrl: base, llmApiKey: 'isolated-fixture', llmApiModel: 'gemini-3.7-flash', videoApiUrl: '', videoApiKey: '', videoApiModel: 'kling-video-v3', outputDir: path.join(auditRoot, 'data', 'assets') } });
    const active = await invoke('db_select', { query: 'SELECT value FROM settings WHERE key=?', bindValues: ['active_project'] });
    const projectId = active[0].value;
    scope = await makeScope(projectId, 'AI优化与漫画注入', 'ai-main'); const mainScope = scope;
    const original = await save('settings', settings, '初始要求：保留人物身份。');
    assert.equal(original.optimizationInstruction, '初始要求：保留人物身份。');
    assert.equal((await mockState()).requests.length, 0); check('save Markdown optimization instruction metadata without model call');
    await openScope(scope); await nav('作品设定');
    const draftBody = `${settings}\n\n手工草稿身份：DRAFT_BEFORE_OPTIMIZE`, instruction = '增强夜雨氛围，同时保留人物身份。';
    await setField('作品设定 Markdown', draftBody); await setField(labels.instruction, instruction);
    const optimizedBody = `${settings}\n\n优化结果身份：SETTINGS_AI_RESULT`;
    await control({ markdown: optimizedBody }); const before = await workspace(), ids = new Set(before.jobs.map(j => j.id));
    await click(labels.optimize); let result = await afterUiJob('optimize', ids);
    let current = getDoc(result.ws, 'settings'); assert.equal(current.markdown, optimizedBody); assert.equal(current.optimizationInstruction, instruction);
    const firstRequest = (await textRequests()).at(-1); assert(requestText(firstRequest).includes(draftBody)); assert(requestText(firstRequest).includes(instruction));
    const versions = await history(current); assert(versions.some(v => v.markdown === draftBody && v.optimizationInstruction === instruction));
    assert(versions.some(v => v.revision === original.revision && v.optimizationInstruction === original.optimizationInstruction));
    check('real AI button saves current draft and instruction then optimizes that exact revision', { revisions: versions.map(v => v.revision), instruction });
    await until(async () => await fieldValue('作品设定 Markdown') === optimizedBody, 'UI shows finished optimization');
    await click(labels.prior);
    assert.equal(await fieldValue('作品设定 Markdown'), draftBody);
    assert.equal(await fieldValue(labels.versions), String(current.revision - 1));
    await click(labels.current);
    check('previous version from current goes to the truly older revision');

    const unsavedBody = `${optimizedBody}\n\n未保存当前草稿：KEEP_ME`, unsavedInstruction = '尚未保存的要求：KEEP_INSTRUCTION';
    await setField('作品设定 Markdown', unsavedBody); await setField(labels.instruction, unsavedInstruction);
    await setField(labels.versions, String(original.revision), 'select');
    assert.equal(await fieldValue('作品设定 Markdown'), original.markdown); assert.equal(await fieldValue(labels.instruction), original.optimizationInstruction);
    await click(labels.next); assert.equal(await fieldValue('作品设定 Markdown'), draftBody); assert.equal(await fieldValue(labels.instruction), instruction);
    await click(labels.prior); assert.equal(await fieldValue('作品设定 Markdown'), original.markdown);
    await click(labels.current); assert.equal(await fieldValue('作品设定 Markdown'), unsavedBody); assert.equal(await fieldValue(labels.instruction), unsavedInstruction);
    check('history selector and previous next synchronize Markdown and instructions while preserving current draft');
    await setField(labels.versions, String(original.revision), 'select'); await click(labels.adopt);
    current = await until(async () => { const d = getDoc(await workspace(), 'settings'); return d.revision > result.ws.documents.find(d => d.kind === 'settings').revision ? d : null; }, 'adopt history creates new revision');
    assert.equal(current.markdown, original.markdown); assert.equal(current.optimizationInstruction, original.optimizationInstruction);
    assert((await history(current)).some(v => v.revision === original.revision && v.markdown === original.markdown));
    check('adopting a historical version creates a new revision without altering old history');
    await setField(labels.versions, String(original.revision), 'select');
    const historyOutput = `${settings}\n\n从历史版本优化后的新结果：HISTORY_OPTIMIZED`;
    await control({ markdown: historyOutput });
    const historicalIds = new Set((await workspace()).jobs.map(j => j.id));
    await click(labels.optimize); result = await afterUiJob('optimize', historicalIds);
    await until(async () => await fieldValue('作品设定 Markdown') === historyOutput, 'historical AI result displayed instead of pinned historical text');
    assert.equal(await fieldValue(labels.instruction), original.optimizationInstruction);
    await click(labels.current);
    assert.equal(await fieldValue('作品设定 Markdown'), unsavedBody);
    assert.equal(await fieldValue(labels.instruction), unsavedInstruction);
    check('optimization launched from history displays its new result while the current unsaved draft remains recoverable');

    let scriptDoc = await save('script', script, '剧本初始要求');
    result = await settled(await optimizeDoc(scriptDoc, '对白更简练', `${script}\n\n剧本优化完成。`)); scriptDoc = getDoc(result.ws, 'script');
    assert.equal(scriptDoc.optimizationInstruction, '对白更简练'); check('script Markdown supports AI optimization and instruction history');
    const beforeHold = (await textRequests()).length;
    const raceOutput = `${script}\n\n这是迟到AI候选。`, manualOutput = `${script}\n\n这是用户在优化途中保存的正文。`;
    const raceJob = await optimizeDoc(scriptDoc, '迟到要求', raceOutput, { holdText: true });
    await until(async () => { const state = await mockState(); return state.requests.filter(r => r.kind === 'text').length === beforeHold + 1 && state.heldTexts === 1; }, 'held optimizer reached actual mock');
    const manual = await save('script', manualOutput, '用户同时保存的要求'); await control({ releaseText: true });
    result = await settled(raceJob, 'failed'); assert.equal(getDoc(result.ws, 'script').revision, manual.revision); assert.equal(getDoc(result.ws, 'script').markdown, manualOutput); assert(result.job.outputMarkdown?.includes(raceOutput));
    check('late optimization result fails CAS and preserves concurrent manual Markdown and instruction');
    let storyboardDoc = await save('storyboard', storyboard.split('# 第4页')[0].trim(), '分镜初始要求');
    result = await settled(await optimizeDoc(storyboardDoc, '让分页更清楚', `${storyboard}\n\n分页规划已优化。`)); assert.equal(getDoc(result.ws, 'storyboard').optimizationInstruction, '让分页更清楚');
    assert(getDoc(result.ws,'storyboard').markdown.includes('# 第4页')); assert.equal(getDoc(result.ws,'storyboard').issues.length,0);
    check('storyboard Markdown optimization can change three pages into four valid contiguous pages');
    for (let n=1;n<=4;n++) await save('page_prompt', pageText(n), `第${n}页原要求`, n);
    await nav('Prompt');
    await until(async () => await fieldValue('本页 Prompt Markdown') === pageText(1), 'page editor hydrated with saved page one');
    const batchInstruction = '每一页保持人物一致，增强雨夜光影。'; await setField(labels.instruction, batchInstruction);
    await evaluate(`(() => { const input=[...document.querySelectorAll('label')].find(l=>l.textContent.trim()===${JSON.stringify(labels.all)})?.querySelector('input[type="checkbox"]'); if(!input)throw new Error('No optimize all checkbox'); if(!input.checked)input.click(); })()`);
    const beforeBatch = await workspace(), batchIds = new Set(beforeBatch.jobs.map(j=>j.id)), textStart = (await textRequests()).length;
    for(let n=1;n<=4;n++) await control({markdown:`${pageText(n)}\n\n全页优化结果：OPTIMIZED_${n}`});
    await click(labels.optimize); result = await afterUiJob('optimize', batchIds);
    const batchRequests = (await textRequests()).slice(textStart); assert.equal(batchRequests.length,4);
    for(let n=1;n<=4;n++){ const output=getDoc(result.ws,'page_prompt',n); assert(output.markdown.includes(`OPTIMIZED_${n}`)); assert.equal(output.optimizationInstruction,batchInstruction); assert(requestText(batchRequests[n-1]).includes(pageText(n))); assert(requestText(batchRequests[n-1]).includes(batchInstruction)); }
    check('actual optimize all checkbox sends each of four page documents to the provider with the requested instruction');
    const beforeTruncate = [1,2,3,4].map(pageNo=>getDoc(result.ws,'page_prompt',pageNo)), first = beforeTruncate[0], failedRaw = `${pageText(2)}\n\n节点齐全但输出被截断。`, truncateCount = (await textRequests()).length;
    await control({markdown:`${pageText(1)}\n\n批量第一页已完成。`}); await control({markdown:failedRaw,textMode:'sse_length'});
    result=await settled(await invoke('comic_md_optimize',{input:{...scope,targets:beforeTruncate.map(document=>({documentId:document.id,revision:document.revision})),instruction:'测试批量中途截断',allPages:true}}),'failed');
    assert.equal(result.job.completedPages,1); assert(getDoc(result.ws,'page_prompt',1).revision>first.revision); for(const document of beforeTruncate.slice(1))assert.equal(getDoc(result.ws,'page_prompt',document.pageNo).revision,document.revision); assert(result.job.outputMarkdown?.includes(failedRaw));
    await pause(350); assert.equal((await textRequests()).length, truncateCount + 2, 'Failed optimizer must not resubmit automatically');
    check('batch optimization truncation retains completed page raw failed output and untouched remaining page');

    await nav('漫画'); const injection='所有对白必须使用中文。若页Prompt要求英文，以本规则为准。';
    await setField(labels.injection,injection); await click(labels.saveRules);
    let ws=await until(async()=>{const value=await workspace();return value.renderOptions?.promptInjection===injection?value:null;},'saved comic injection');
    check('real comic rules editor saves versioned prompt injection',ws.renderOptions);
    const optionsCount=(await mockState()).requests.length;
    await assert.rejects(() => invoke('comic_md_render_options_save', {input:{...scope,promptInjection:'迟到规则',expectedRevision:ws.renderOptions.revision-1}}));
    await assert.rejects(() => invoke('comic_md_render', {input:{...scope,pages:[{documentId:getDoc(ws,'page_prompt',1).id,revision:getDoc(ws,'page_prompt',1).revision}],expectedRenderOptionsRevision:ws.renderOptions.revision-1}}));
    assert.deepEqual((await workspace()).renderOptions,ws.renderOptions); assert.equal((await mockState()).requests.length,optionsCount);
    check('stale injection save and render revisions reject without overwriting options or contacting provider');
    ws=await uiRender('生成第一页',[1]); ws=await uiRender('生成前三页',[1,2,3]); ws=await uiRender('生成剩余页',[4]);
    assert(ws.images.every(image=>image.promptInjection===injection));
    const beforeNoop=(await imageRequests()).length;
    await until(() => evaluate(`([...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='生成剩余页'))?.disabled===true`), 'all current images reflected by disabled remaining button');
    assert.equal((await imageRequests()).length,beforeNoop); check('remaining pages action skips all current successful document and injection matches');
    const pageTwo=getDoc(ws,'page_prompt',2); await save('page_prompt',`${pageTwo.markdown}\n\n新修订重画第二页。`,pageTwo.optimizationInstruction,2); await pause(3300);
    ws=await uiRender('生成剩余页',[2]);
    const injection2='所有对白必须使用中文；整章统一黑白漫画。'; await setField(labels.injection,injection2); await click(labels.saveRules);
    ws=await until(async()=>{const value=await workspace();return value.renderOptions.promptInjection===injection2?value:null;},'updated injection'); assert(ws.images.every(image=>image.stale));
    check('changing injection marks previous images stale without deleting them');
    await uiRender('生成剩余页',[1,2,3,4]);
    await setField(labels.injection,injection);
    const beforeReturn=(await imageRequests()).length;
    await until(() => evaluate(`([...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='生成剩余页'))?.disabled===true`), 'returning injection draft A recognizes existing A images');
    await evaluate(`([...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='生成剩余页')).click()`);
    await pause(300); assert.equal((await imageRequests()).length,beforeReturn);
    await click(labels.saveRules);
    await until(async()=>(await workspace()).renderOptions.promptInjection===injection,'A saved again');
    assert.equal((await imageRequests()).length,beforeReturn);
    check('changing injection B back to A reuses matching successful A images without another image request');

    const twoPageScope=await makeScope(projectId,'仅有两页的漫画','ai-two-pages'); scope=twoPageScope;
    await save('page_prompt',pageText(1),'',1); await save('page_prompt',pageText(2),'',2); await openScope(scope); await nav('漫画'); await uiRender('生成前三页',[1,2]);
    const gapScope=await makeScope(projectId,'缺第二页的漫画','ai-gap'); scope=gapScope;
    for(const n of [1,3,4])await save('page_prompt',pageText(n),'',n); await openScope(scope); await nav('漫画');
    assert.equal(await evaluate(`([...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='生成前三页'))?.disabled`),true);
    const gapCount=(await imageRequests()).length;
    await evaluate(`([...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='生成前三页')).click()`);
    assert.equal((await imageRequests()).length,gapCount);
    check('first three page action blocks an internal missing page and never substitutes page four');
    await save('page_prompt','# 第2页\n不完整草稿','',2); await pause(3300);
    const invalidButton = await evaluate(`[...document.querySelectorAll('button')].find(b=>b.textContent.trim().startsWith('生成前三页'))?.disabled`);
    assert.equal(invalidButton,true); assert.equal((await imageRequests()).length,gapCount);
    check('first three page action blocks an existing but incomplete second page');
    await openScope(mainScope); await nav('Prompt'); await capture('ai-versions-default.png');
    await evaluate(`document.querySelector('textarea[aria-label="AI 优化要求"]')?.scrollIntoView({block:'end'})`); await capture('ai-editor-default.png');
    await send('Emulation.setDeviceMetricsOverride',{width:960,height:640,deviceScaleFactor:1,mobile:false}); await pause(200);
    await evaluate(`document.querySelector('textarea[aria-label="AI 优化要求"]')?.scrollIntoView({block:'end'})`); await capture('ai-editor-960x640.png');
    assert.equal(await evaluate(`document.documentElement.scrollWidth>innerWidth+1`),false); await send('Emulation.clearDeviceMetricsOverride');
    await nav('漫画'); await capture('ai-comic-rules-default.png');
    ws=await workspace(); await writeFile(path.join(auditRoot,'ai-checkpoint.json'),JSON.stringify({scope:mainScope,documents:ws.documents,renderOptions:ws.renderOptions,images:ws.images,providerCount:(await mockState()).requests.length},null,2));
    check('AI metadata and injection checkpoint prepared for actual process restart');
  }
  report.status='passed';
} catch(error) {
  report.status='failed';report.error=String(error.stack??error);process.exitCode=1;console.error(report.error);
  try{report.visibleText=await evaluate('document.body.innerText');await capture(`ai-failure-${phase}.png`);}catch(captureError){report.captureError=String(captureError);}
} finally {
  report.finishedAt=new Date().toISOString(); await writeFile(path.join(auditRoot,`acceptance-${phase}.json`),JSON.stringify(report,null,2));socket.close();
}
