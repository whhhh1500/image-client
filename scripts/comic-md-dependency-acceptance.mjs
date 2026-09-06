import assert from 'node:assert/strict';
import { readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { settings, script, pagePrompt } from './comic-md-fixtures.mjs';

const [cdpText, mockText, auditRoot, phase = 'initial'] = process.argv.slice(2);
assert(path.isAbsolute(auditRoot));
const report = { phase, scenario: 'dependency', checks: [], startedAt: new Date().toISOString() };
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
const board = count => Array.from({ length: count }, (_, i) => `# 第${i+1}页\n## 本页剧情\n林青${i+1}次观察木门。\n## 分镜\n### 第1格\n林青右手持信看向木门。\n## 画面文字\n无对白。\n## 人物状态\n左臂包扎，右手持信。`).join('\n\n');
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

const assertReady = ws => assert(ws.documents.filter(d=>!d.outOfPlan).every(d=>!d.stale && d.issues.length===0), JSON.stringify(ws.documents));
async function seedChapter(pageCount=3) {
  await save('settings',settings,'初始设定要求'); await save('script',script,'初始剧本要求'); await save('storyboard',board(pageCount),'初始分镜要求');
  for(let n=1;n<=pageCount;n++)await save('page_prompt',pageText(n),`第${n}页初始要求`,n);
  const ws=await workspace();assertReady(ws);return ws;
}
async function queueOutputs(outputs) { await control({clearTextQueue:true}); for(const output of outputs)await control({markdown:output}); }
async function submitSync() {
  const ws=await workspace(); assert(!ws.syncPlan.blockedReason,JSON.stringify(ws.syncPlan));
  return invoke('comic_md_sync',{input:{...scope,expectedPlanFingerprint:ws.syncPlan.fingerprint}});
}
async function syncWithOutputs(outputs, viaUi=false) {
  await queueOutputs(outputs);const before=await workspace(),start=(await textRequests()).length;
  let result;
  if(viaUi){await click('更新本章受影响文字');result=await afterUiJob('sync',new Set(before.jobs.map(j=>j.id)));}
  else result=await settled(await submitSync());
  const requests=(await textRequests()).slice(start);assert.equal(requests.length,outputs.length,'One real provider call per sequential target');
  assert.equal(result.ws.syncPlan.targets.length,0,'Successful full sync must leave no affected target in this chapter');assert.deepEqual(result.ws.syncPlan.missingPageNos,[],'Successful full sync must leave no missing page');
  return {...result,requests};
}

try {
  await until(()=>evaluate(`Boolean(window.__TAURI_INTERNALS__?.invoke && document.querySelector('#root')?.children.length)`),'frontend ready');await pause(1200);
  if(phase==='restore'){
    const prior=JSON.parse(await readFile(path.join(auditRoot,'dependency-checkpoint.json'),'utf8'));scope=prior.scope;
    const ws=await workspace();assert.deepEqual(ws.documents,prior.documents);assert.deepEqual(ws.images,prior.images);assert.deepEqual(ws.syncPlan,prior.syncPlan);assert.deepEqual(ws.affectedChapters,prior.affectedChapters);
    await pause(500);assert.equal((await mockState()).requests.length,prior.providerCount);
    check('restart preserves content hashes dependency reasons sync plan image provenance and affected chapters without provider resubmission');
    await openScope(scope);await nav('Prompt');await capture('dependency-restored.png');
  }else{
    const base=`http://127.0.0.1:${mockText}/v1`;
    await invoke('save_config',{config:{imageApiUrl:`${base}/images/generations`,imageApiKey:'isolated-fixture',imageApiModel:'gpt-image-2',llmApiUrl:base,llmApiKey:'isolated-fixture',llmApiModel:'gemini-3.7-flash',videoApiUrl:'',videoApiKey:'',videoApiModel:'kling-video-v3',outputDir:path.join(auditRoot,'data','assets')}});
    const active=await invoke('db_select',{query:'SELECT value FROM settings WHERE key=?',bindValues:['active_project']});const projectId=active[0].value;
    scope=await makeScope(projectId,'关联更新验收','dependency-main');const mainScope=scope;
    let ws=await seedChapter();await openScope(scope);await nav('漫画');
    ws=await uiRender('生成前三页',[1,2,3]);
    const missingAsset=ws.images.find(image=>image.pageNo===2&&image.fileAvailable!==false);assert(missingAsset);
    const auditPrefix=`${path.resolve(auditRoot)}${path.sep}`;assert(path.resolve(missingAsset.path).startsWith(auditPrefix),'Only the isolated audit asset may be removed');
    await rm(missingAsset.path);ws=await until(async()=>{const value=await workspace();return value.images.some(image=>image.id===missingAsset.id&&image.fileAvailable===false)?value:null;},'missing image file reflected by workspace');
    await pause(3300);ws=await uiRender('生成剩余页',[2]);
    check('remaining pages regenerates a database image whose actual file is missing',{page:2,missingPath:missingAsset.path});
    const p1=getDoc(ws,'page_prompt',1),settingsBefore=getDoc(ws,'settings'),requestsBefore=(await mockState()).requests.length;
    await nav('Prompt');await until(async()=>await fieldValue('本页 Prompt Markdown')===p1.markdown,'saved page text');
    await setField(labels.instruction,'只改优化要求，不改任何正文。');await click('保存文字');
    ws=await until(async()=>{const value=await workspace();return getDoc(value,'page_prompt',1).revision>p1.revision?value:null;},'instruction-only new revision');
    assert.equal(getDoc(ws,'page_prompt',1).contentHash,p1.contentHash);assertReady(ws);
    assert([1,2,3].every(pageNo=>ws.images.some(image=>image.pageNo===pageNo&&!image.stale&&image.fileAvailable!==false&&image.contentHash===getDoc(ws,'page_prompt',pageNo).contentHash)));
    await save('settings',settingsBefore.markdown,'只改设定优化要求。');ws=await workspace();assert.equal(getDoc(ws,'settings').contentHash,settingsBefore.contentHash);assertReady(ws);
    assert([1,2,3].every(pageNo=>ws.images.some(image=>image.pageNo===pageNo&&!image.stale&&image.fileAvailable!==false&&image.contentHash===getDoc(ws,'page_prompt',pageNo).contentHash)));
    await nav('漫画');await until(()=>evaluate(`([...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='生成剩余页'))?.disabled===true`),'instruction-only remaining disabled');
    await evaluate(`([...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='生成剩余页')).click()`);assert.equal((await mockState()).requests.length,requestsBefore);
    check('actual instruction-only save creates revision with equal contentHash and keeps downstream images current without repeat rendering');

    const settingChanged=`${settings}\n\n环境补充：SETTING_CONTEXT_NEW，客栈后院新增蓝灯。`;
    await save('settings',settingChanged,'保持蓝灯连续性');ws=await workspace();
    assert(ws.documents.some(d=>d.kind==='script'&&d.stale&&d.staleReasons.length));assert(ws.syncPlan.targets.length>=5);
    check('settings content change exposes concrete stale reasons and ordered affected-document plan',ws.syncPlan);
    const staleScript=getDoc(ws,'script');await save('script',staleScript.markdown,'仅改要求不代表已核对上游');ws=await workspace();
    assert(getDoc(ws,'script').stale);assert.equal(getDoc(ws,'script').contentHash,staleScript.contentHash);
    check('instruction-only edit of an already stale document does not acknowledge or clear unresolved dependencies');
    const staleBody=getDoc(ws,'script'),editedStaleBody=`${staleBody.markdown}\n\n用户手工修订：STALE_BODY_MUST_STAY_PENDING`;
    await save('script',editedStaleBody,staleBody.optimizationInstruction);ws=await workspace();
    assert.equal(getDoc(ws,'script').markdown,editedStaleBody);assert(getDoc(ws,'script').stale);assert(getDoc(ws,'script').staleReasons.length);
    check('ordinary body save of a stale document preserves its unresolved dependency receipt until explicit acknowledgement');
    // An optimization of a downstream page must not erase unresolved upstream staleness.
    const stalePage=getDoc(ws,'page_prompt',1),beforeStaleRevision=stalePage.revision;
    await queueOutputs([`${stalePage.markdown}\n\n不能洗白上游的候选。`]);let rejected=false;
    try{const job=await invoke('comic_md_optimize',{input:{...scope,targets:[{documentId:stalePage.id,revision:stalePage.revision}],instruction:'直接优化这个过期页面'}});await settled(job,'failed');}catch(error){rejected=true;}
    const stillStale=getDoc(await workspace(),'page_prompt',1);assert(stillStale.stale);assert.equal(stillStale.revision,beforeStaleRevision);await control({clearTextQueue:true});
    check('direct downstream optimization cannot clear stale upstream dependencies',{admissionRejected:rejected,staleReasons:stillStale.staleReasons});
    const oldFingerprint=ws.syncPlan.fingerprint;await save('settings',`${settingChanged}\n计划修订：FINGERPRINT_NEW`,'保持蓝灯连续性');
    await assert.rejects(()=>invoke('comic_md_sync',{input:{...scope,expectedPlanFingerprint:oldFingerprint}}));check('outdated sync plan fingerprint rejects changed inputs');
    const syncedScript=`${script}\n\n关联剧本：SYNC_SCRIPT_NEW`,syncedBoard=`${board(3)}\n\n关联分镜：SYNC_BOARD_NEW`;
    const syncedPages=[1,2,3].map(n=>pageText(n).replace('左臂受伤用布条包扎',`左臂受伤用蓝布包扎，当前页状态 SYNC_STATE_${n}`));
    await nav('作品设定');const synced=await syncWithOutputs([syncedScript,syncedBoard,...syncedPages],true);ws=synced.ws;assertReady(ws);
    assert(requestText(synced.requests[0]).includes('SETTING_CONTEXT_NEW'));assert(requestText(synced.requests[1]).includes('SYNC_SCRIPT_NEW'));
    assert(requestText(synced.requests[2]).includes('SYNC_BOARD_NEW'));assert(requestText(synced.requests[3]).includes('SYNC_STATE_1'));assert(requestText(synced.requests[4]).includes('SYNC_STATE_2'));
    check('actual one-click sync updates script storyboard and each page in sequence using latest upstream and just-updated prior-page context');
    await nav('漫画');ws=await uiRender('生成前三页',[1,2,3]);

    const cameraOld=getDoc(ws,'page_prompt',1);await save('page_prompt',cameraOld.markdown.replace('远景。','远景：LOCAL_CAMERA_ONLY。'),cameraOld.optimizationInstruction,1);ws=await workspace();
    assert(!getDoc(ws,'page_prompt',2).stale);assert(!getDoc(ws,'page_prompt',3).stale);
    assert(ws.images.filter(i=>i.pageNo===1).every(i=>i.stale));assert(ws.images.some(i=>i.pageNo===2&&!i.stale));assert(ws.images.some(i=>i.pageNo===3&&!i.stale));
    check('camera-only page edit only stales its own image and does not stale later pages or their current images');
    const stateOld=getDoc(ws,'page_prompt',1);await save('page_prompt',stateOld.markdown.replace('SYNC_STATE_1','STATE_CHANGE_PAGE_ONE'),stateOld.optimizationInstruction,1);ws=await workspace();
    assert(getDoc(ws,'page_prompt',2).stale&&getDoc(ws,'page_prompt',2).staleReasons.length);assert(getDoc(ws,'page_prompt',3).stale);
    await nav('Prompt');await pause(3300);await evaluate(`(()=>{const item=[...document.querySelectorAll('summary')].find(s=>s.textContent.includes('查看受影响内容与原因'));if(item&&!item.parentElement.open)item.click();})()`);await capture('dependency-state-impact.png');
    check('character-state edit marks later page dependencies stale with reasons',ws.syncPlan);
    const stateTwo=syncedPages[1].replace('SYNC_STATE_2','STATE_SYNC_TWO'),stateThree=syncedPages[2].replace('SYNC_STATE_3','STATE_SYNC_THREE');
    const stateSync=await syncWithOutputs([stateTwo,stateThree]);assertReady(stateSync.ws);assert(requestText(stateSync.requests[0]).includes('STATE_CHANGE_PAGE_ONE'));assert(requestText(stateSync.requests[1]).includes('STATE_SYNC_TWO'));
    check('state synchronization feeds page two new output into page three request');

    // The ordinary optimize-all path must use the same sequential dependency context as sync.
    await nav('Prompt');
    // IPC sync updates pages 2/3 while page 1 stays equal; verify those changed DOM values before freezing UI revisions.
    for(const n of [2,3,1]){
      await setField('Prompt 页码',String(n),'input');
      const expected=getDoc(stateSync.ws,'page_prompt',n);
      await until(async()=>await fieldValue('本页 Prompt Markdown')===expected.markdown,`page ${n} DOM refreshed after direct IPC sync`);
    }
    await setField(labels.instruction,'顺序优化，保持前页刚生成的人物状态。');
    await evaluate(`(()=>{const box=[...document.querySelectorAll('label')].find(l=>l.textContent.trim()==='优化全部页')?.querySelector('input');if(!box)throw new Error('Missing optimize-all checkbox');if(!box.checked)box.click();})()`);
    const batchOutputs=[1,2,3].map(n=>pageText(n).replace('左臂受伤用布条包扎',`左臂受伤用蓝布包扎，BATCH_STATE_${n}`));await queueOutputs(batchOutputs);
    const batchBefore=await workspace(),batchStart=(await textRequests()).length;await click('AI 优化');const batchResult=await afterUiJob('optimize',new Set(batchBefore.jobs.map(j=>j.id)));
    const batchRequests=(await textRequests()).slice(batchStart);assert.equal(batchRequests.length,3);assert(requestText(batchRequests[1]).includes('BATCH_STATE_1'));assert(requestText(batchRequests[2]).includes('BATCH_STATE_2'));assertReady(batchResult.ws);
    check('actual optimize-all checkbox feeds each newly optimized character state into the next page request');

    let head=getDoc(batchResult.ws,'page_prompt',1);await save('page_prompt',head.markdown.replace('BATCH_STATE_1','CAS_STATE_NEW'),head.optimizationInstruction,1);
    const holdStart=(await textRequests()).length,lateOutput=batchOutputs[1].replace('BATCH_STATE_2','LATE_SYNC_CANDIDATE');await control({clearTextQueue:true});await control({markdown:lateOutput,holdText:true});
    const holdJob=await submitSync();await until(async()=>{const state=await mockState();return state.heldTexts===1&&state.requests.filter(r=>r.kind==='text').length===holdStart+1;},'sync provider response held');
    const manual=await save('page_prompt',batchOutputs[1].replace('BATCH_STATE_2','MANUAL_DURING_SYNC'),'用户在同步期间保存',2);await control({releaseText:true});
    const late=await settled(holdJob,'failed');assert.equal(getDoc(late.ws,'page_prompt',2).revision,manual.revision);assert.equal(getDoc(late.ws,'page_prompt',2).markdown,manual.markdown);assert(late.job.outputMarkdown?.includes('LATE_SYNC_CANDIDATE'));
    assert.equal((await textRequests()).length,holdStart+1);check('held sync result cannot overwrite an external user save and retains rejected raw output');

    head=getDoc(late.ws,'page_prompt',1);await save('page_prompt',head.markdown.replace('CAS_STATE_NEW','PARTIAL_STATE_NEW'),head.optimizationInstruction,1);
    const partialBefore=await workspace(),thirdBefore=getDoc(partialBefore,'page_prompt',3),partialStart=(await textRequests()).length;
    const partialTwo=batchOutputs[1].replace('BATCH_STATE_2','PARTIAL_COMPLETED_TWO'),partialThree=batchOutputs[2].replace('BATCH_STATE_3','PARTIAL_FAILED_THREE');
    await control({markdown:partialTwo});await control({markdown:partialThree,textMode:'sse_length'});
    const partial=await settled(await submitSync(),'failed');assert.equal(partial.job.completedPages,1);assert.equal(getDoc(partial.ws,'page_prompt',2).markdown,partialTwo);assert.equal(getDoc(partial.ws,'page_prompt',3).revision,thirdBefore.revision);assert(partial.job.outputMarkdown?.includes('PARTIAL_FAILED_THREE'));
    await pause(350);assert.equal((await textRequests()).length,partialStart+2);check('partial sync failure preserves completed page and failed raw output without overwriting or retrying the unfinished page');
    await syncWithOutputs([batchOutputs[2].replace('BATCH_STATE_3','RECOVERED_THREE')]);

    await save('storyboard',board(2),'减少为两页');ws=await workspace();const obsolete=getDoc(ws,'page_prompt',3);
    assert(obsolete.outOfPlan);assert(ws.syncPlan.obsoletePageNos.includes(3));
    const outRequestCount=(await mockState()).requests.length;
    await assert.rejects(()=>invoke('comic_md_render',{input:{...scope,pages:[{documentId:obsolete.id,revision:obsolete.revision}],expectedRenderOptionsRevision:ws.renderOptions.revision}}));assert.equal((await mockState()).requests.length,outRequestCount);
    const exported=await invoke('comic_md_export',{input:scope});assert.equal(exported.files.length,5);for(const file of exported.files){const text=await readFile(file,'utf8');assert(!text.includes('RECOVERED_THREE'));}
    const legacyExport=await invoke('comic_md_export',{input:{...scope,documentIds:[obsolete.id]}});assert.equal(await readFile(legacyExport.files[0],'utf8'),obsolete.markdown);
    await nav('Prompt');await setField('Prompt 页码','3','input');await until(async()=>await fieldValue('本页 Prompt Markdown')===obsolete.markdown,'out-of-plan old page remains viewable');
    await until(()=>evaluate(`([...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='生成这一页漫画'))?.disabled===true`),'out-of-plan single-page UI render disabled');
    check('reduced storyboard preserves out-of-plan page but rejects its rendering and excludes it from default export while explicit historical export remains available');
    await syncWithOutputs([pageText(1),pageText(2)]);
    await nav('漫画');await uiRender('生成前三页',[1,2]);
    await save('storyboard',board(4),'增加为四页');ws=await workspace();assert(ws.syncPlan.missingPageNos.includes(4));assert(!getDoc(ws,'page_prompt',3).outOfPlan);
    const expanded=await syncWithOutputs([1,2,3,4].map(pageText));assertReady(expanded.ws);assert.equal(expanded.ws.documents.filter(d=>d.kind==='page_prompt'&&!d.outOfPlan).length,4);assert.deepEqual(expanded.ws.syncPlan.missingPageNos,[]);
    check('expanded storyboard reuses preserved pages and creates missing page four through sequential sync');

    const sourceTwo=await invoke('novel_chapter_revision_create',{input:{projectId,novelWorkId:scope.novelWorkId,chapterNo:2,title:'后章关联检查',content:'林青接着穿过雨中的长廊。',idempotencyKey:'dependency-chapter-two'}});
    const secondScope={...scope,chapterId:sourceTwo.chapterId};scope=secondScope;
    await save('script',`${script}\n\n第二章剧情。`,'第二章要求');await save('storyboard',board(2),'第二章分页');for(const n of [1,2])await save('page_prompt',pageText(n),'第二章Prompt',n);
    assertReady(await workspace());scope=mainScope;head=getDoc(await workspace(),'page_prompt',4);await save('page_prompt',head.markdown.replace('左臂受伤用布条包扎','左臂受伤改用红布包扎，CROSS_CHAPTER_STATE'),head.optimizationInstruction,4);ws=await workspace();
    assert(ws.affectedChapters.some(c=>c.chapterId===secondScope.chapterId&&c.documentCount>0&&c.reason));
    await openScope(mainScope);await nav('Prompt');
    await evaluate(`(()=>{const item=[...document.querySelectorAll('summary')].find(s=>s.textContent.includes('本书另有'));if(!item)throw new Error('No affected chapters summary');if(!item.parentElement.open)item.click();})()`);
    await until(()=>evaluate(`document.body.innerText.includes('后章关联检查')`),'affected chapter visible');
    const beforeJump=(await mockState()).requests.length;
    await click('前往第2章处理');
    await until(()=>evaluate(`document.querySelector('select[aria-label="选择章节"]')?.value===${JSON.stringify(secondScope.chapterId)}`),'affected chapter selected');assert.equal((await mockState()).requests.length,beforeJump);
    check('cross-chapter character-state impact is visible and actual navigation opens the affected chapter without automatic provider dispatch');
    scope=mainScope;const unrelatedTarget=getDoc(await workspace(),'page_prompt',1),unrelatedOutput=`${unrelatedTarget.markdown}\n未来章节无关编辑不应中断：FUTURE_UNRELATED_OK`;
    const unaffectedJob=await optimizeDoc(unrelatedTarget,'镜头文字保持清晰',unrelatedOutput,{holdText:true});await until(async()=>(await mockState()).heldTexts===1,'current chapter optimization held');
    scope=secondScope;const futureScript=getDoc(await workspace(),'script');await save('script',futureScript.markdown,'未来章节只改优化要求');scope=mainScope;await control({releaseText:true});
    const unaffected=await settled(unaffectedJob);assert.equal(getDoc(unaffected.ws,'page_prompt',1).markdown,unrelatedOutput);check('editing only an unrelated future chapter instruction does not interrupt current chapter optimization');

    const gapScope=await makeScope(projectId,'先补中间缺页','dependency-middle-gap');scope=gapScope;
    await save('settings',settings);await save('script',script);await save('storyboard',board(3));await save('page_prompt',pageText(1),'',1);await save('page_prompt',pageText(3),'',3);
    ws=await workspace();assert(ws.syncPlan.missingPageNos.includes(2));
    await openScope(gapScope);await nav('Prompt');await setField('Prompt 页码','3','input');await until(async()=>await fieldValue('本页 Prompt Markdown')===getDoc(ws,'page_prompt',3).markdown,'gap page three editor ready');
    const gapPageThree=getDoc(ws,'page_prompt',3),gapDraft=`${gapPageThree.markdown}\n\n未保存草稿：DO_NOT_OVERWRITE_FORWARD_PAGE`;
    await setField('本页 Prompt Markdown',gapDraft);
    await until(()=>evaluate(`document.body.innerText.includes('第3页 Prompt 可能被本次联动更新')`),'forward draft sync warning');
    assert.equal(await evaluate(`([...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='更新本章受影响文字'))?.disabled`),true);
    check('missing middle page blocks sync when an existing later page has an unsaved draft');
    await setField(labels.instruction,'缺页时不允许伪装成优化全部页。');
    await evaluate(`(()=>{const box=[...document.querySelectorAll('label')].find(l=>l.textContent.trim()==='优化全部页')?.querySelector('input');if(!box)throw new Error('Missing optimize-all checkbox');if(!box.checked)box.click();})()`);
    await until(()=>evaluate(`document.body.innerText.includes('分镜仍缺少第2页 Prompt，请先补齐后再优化全部页')`),'optimize-all missing page warning');
    assert.equal(await evaluate(`([...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='AI 优化'))?.disabled`),true);
    const gapTextBefore=(await textRequests()).length;
    await assert.rejects(()=>invoke('comic_md_optimize',{input:{...scope,targets:[1,3].map(pageNo=>{const document=getDoc(ws,'page_prompt',pageNo);return{documentId:document.id,revision:document.revision};}),instruction:'绕过界面尝试优化全部页',allPages:true}}));
    assert.equal((await textRequests()).length,gapTextBefore);
    check('optimize-all is blocked in both UI and backend when any planned page Prompt is missing');
    await setField('本页 Prompt Markdown',gapPageThree.markdown);
    await setField(labels.instruction,gapPageThree.optimizationInstruction ?? '');
    const middleTwo=pageText(2).replace('左臂受伤用布条包扎','左臂受伤用蓝布包扎，MIDDLE_PAGE_TWO_NEW');const middleThree=pageText(3).replace('左臂受伤用布条包扎','左臂受伤用蓝布包扎，MIDDLE_PAGE_THREE_NEW');
    const middle=await syncWithOutputs([middleTwo,middleThree]);assert(requestText(middle.requests[1]).includes('MIDDLE_PAGE_TWO_NEW'));assert.equal(getDoc(middle.ws,'page_prompt',2).markdown,middleTwo);assert.equal(getDoc(middle.ws,'page_prompt',3).markdown,middleThree);assertReady(middle.ws);
    check('sync creates missing page two before updating existing page three exactly once and finishes with an empty chapter plan');
    const countGuardScope=await makeScope(projectId,'联动不得暗改页数','dependency-page-count-guard');scope=countGuardScope;await seedChapter(3);
    const guardSettings=getDoc(await workspace(),'settings');await save('settings',`${guardSettings.markdown}\n\n触发联动：COUNT_GUARD`);const beforeGuard=await workspace(),guardStoryboard=getDoc(beforeGuard,'storyboard'),guardStart=(await textRequests()).length;
    await queueOutputs([`${script}\n\n页数保护剧本`,board(4)]);const guarded=await settled(await submitSync(),'failed');
    assert.equal(guarded.job.completedPages,1);assert.equal(getDoc(guarded.ws,'storyboard').revision,guardStoryboard.revision);assert.equal(getDoc(guarded.ws,'storyboard').markdown,guardStoryboard.markdown);assert(guarded.job.outputMarkdown?.includes('# 第4页'));assert.equal((await textRequests()).length,guardStart+2);assert.equal(getDoc(guarded.ws,'page_prompt',4),undefined);
    check('one-click sync preserves the saved storyboard and stops after raw output when a model changes the page count');
    await openScope(mainScope);await nav('Prompt');await evaluate(`(()=>{const item=[...document.querySelectorAll('summary')].find(s=>s.textContent.includes('本书另有'));if(item&&!item.parentElement.open)item.click();})()`);await capture('dependency-affected-chapters.png');
    await send('Emulation.setDeviceMetricsOverride',{width:960,height:640,deviceScaleFactor:1,mobile:false});await pause(200);await capture('dependency-affected-chapters-960x640.png');assert.equal(await evaluate(`document.documentElement.scrollWidth>innerWidth+1`),false);await send('Emulation.clearDeviceMetricsOverride');
    ws=await workspace();await writeFile(path.join(auditRoot,'dependency-checkpoint.json'),JSON.stringify({scope:mainScope,documents:ws.documents,images:ws.images,syncPlan:ws.syncPlan,affectedChapters:ws.affectedChapters,providerCount:(await mockState()).requests.length},null,2));
    check('dependency checkpoint prepared for actual process restart');
  }
  report.status='passed';
}catch(error){report.status='failed';report.error=String(error.stack??error);process.exitCode=1;console.error(report.error);try{report.visibleText=await evaluate('document.body.innerText');await capture(`dependency-failure-${phase}.png`);}catch(captureError){report.captureError=String(captureError);}}
finally{report.finishedAt=new Date().toISOString();await writeFile(path.join(auditRoot,`acceptance-${phase}.json`),JSON.stringify(report,null,2));socket.close();}
