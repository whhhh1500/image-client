import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

const [cdpPort, auditRoot, phase] = process.argv.slice(2);
if (!cdpPort || !auditRoot || !["initial", "restore"].includes(phase)) {
  throw new Error("Usage: node short-drama-source-acceptance.mjs <cdpPort> <auditRoot> <initial|restore>");
}

const firstMarker = "短剧来源验收第一章：雨夜的信被风吹到门边。";
const marker = "短剧来源验收第二章：门外的脚步声忽然停下。";
const revisedMarker = "短剧来源验收第二章修订：门外的脚步声停下后，信封被雨水浸湿。";
const comicSettings = "# 作品设定\n\n## 世界观\n隔离验收中的雨夜小镇。\n\n## 画风\n黑白手绘。\n\n## 人物锚点\n来信人穿深色雨衣。";
const workTitle = "短剧来源验收小说";
const chapterTitle = "雨夜来信";
const secondChapterTitle = "门外脚步";
const auditPath = join(auditRoot, `short-drama-source-${phase}.json`);

const targets = await (await fetch(`http://127.0.0.1:${cdpPort}/json`)).json();
const page = targets.find((target) => target.type === "page");
if (!page?.webSocketDebuggerUrl) throw new Error("CDP_PAGE_UNAVAILABLE");
const socket = new WebSocket(page.webSocketDebuggerUrl);
let sequence = 0;
const pending = new Map();
socket.addEventListener("message", (event) => {
  const message = JSON.parse(event.data);
  const resolver = pending.get(message.id);
  if (resolver) { pending.delete(message.id); resolver(message); }
});
await new Promise((resolve, reject) => {
  socket.addEventListener("open", resolve, { once: true });
  socket.addEventListener("error", reject, { once: true });
});
const send = (method, params = {}) => new Promise((resolve) => {
  const id = ++sequence;
  pending.set(id, resolve);
  socket.send(JSON.stringify({ id, method, params }));
});
const evaluate = async (expression) => {
  const response = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (response.result?.exceptionDetails) {
    const detail = response.result.exceptionDetails;
    throw new Error(detail.exception?.description ?? detail.exception?.value ?? detail.text);
  }
  return response.result?.result?.value;
};
const captureEvidence = async (label) => {
  try {
    const [text, screenshot] = await Promise.all([
      evaluate("document.body.innerText"),
      send("Page.captureScreenshot", { format: "png" }),
    ]);
    await mkdir(auditRoot, { recursive: true });
    await writeFile(join(auditRoot, `short-drama-source-${phase}-${label}.txt`), String(text));
    if (screenshot.result?.data) await writeFile(join(auditRoot, `short-drama-source-${phase}-${label}.png`), Buffer.from(screenshot.result.data, "base64"));
  } catch {
    // Keep the original failure as the diagnostic when the page is unavailable.
  }
};
const waitFor = async (expression, label, timeout = 30_000) => {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await evaluate(expression)) return;
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  await captureEvidence(`timeout-${label.replace(/[^a-z0-9]+/giu, "-").replace(/^-|-$/gu, "")}`);
  throw new Error(`Timed out waiting for ${label}`);
};
const clickButton = async (label) => {
  const clicked = await evaluate(`(() => {
    const button = [...document.querySelectorAll('button')].find((item) => item.textContent?.trim() === ${JSON.stringify(label)} || item.textContent?.includes(${JSON.stringify(label)}));
    if (!button || button.disabled) return false;
    button.click();
    return true;
  })()`);
  if (!clicked) {
    await captureEvidence(`button-${label.replace(/[^a-z0-9]+/giu, "-").replace(/^-|-$/gu, "")}`);
    throw new Error(`BUTTON_UNAVAILABLE:${label}`);
  }
};
const setControl = async (selector, value) => {
  const changed = await evaluate(`(() => {
    const element = document.querySelector(${JSON.stringify(selector)});
    if (!(element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) || element.disabled) return false;
    const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), 'value')?.set;
    setter?.call(element, ${JSON.stringify(value)});
    element.dispatchEvent(new Event('input', { bubbles: true }));
    element.dispatchEvent(new Event('change', { bubbles: true }));
    return true;
  })()`);
  if (!changed) {
    await captureEvidence("control-unavailable");
    throw new Error(`CONTROL_UNAVAILABLE:${selector}`);
  }
};
const selectControl = async (selector, value) => {
  const changed = await evaluate(`(() => {
    const element = document.querySelector(${JSON.stringify(selector)});
    if (!(element instanceof HTMLSelectElement) || element.disabled || ![...element.options].some((option) => option.value === ${JSON.stringify(value)})) return false;
    const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value')?.set;
    setter?.call(element, ${JSON.stringify(value)});
    element.dispatchEvent(new Event('change', { bubbles: true }));
    return true;
  })()`);
  if (!changed) {
    await captureEvidence("select-unavailable");
    throw new Error(`SELECT_UNAVAILABLE:${selector}`);
  }
};
const selectSourceRow = async () => evaluate(`(async () => {
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  if (!invoke) throw new Error('TAURI_BRIDGE_UNAVAILABLE');
  const rows = await invoke('db_select', { query: "SELECT id, metadata FROM assets WHERE kind='text' ORDER BY created_at DESC", bindValues: [] });
  for (const row of rows) {
    const metadata = JSON.parse(row.metadata ?? '{}');
    const params = metadata.params ?? {};
    if (params.agentId === 'source' && params.novelWorkId && params.novelChapterId && params.novelChapterRevisionId) {
      return { id: row.id, params, text: params.text ?? '' };
    }
  }
  return null;
})()`);
const readChapterRows = async (workId, projectId) => evaluate(`(async () => {
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  if (!invoke) throw new Error('TAURI_BRIDGE_UNAVAILABLE');
  const snapshot = await invoke('novel_work_get', { input: { projectId: ${JSON.stringify(projectId)}, novelWorkId: ${JSON.stringify(workId)} } });
  return snapshot.chapters.map((chapter) => {
    const revision = snapshot.revisions.find((item) => item.id === chapter.latestRevisionId);
    return { chapter_no: chapter.chapterNo, revision_id: revision?.id, revision_no: revision?.revisionNo, content: revision?.content };
  });
})()`);

await waitFor("Boolean(window.__TAURI_INTERNALS__) && document.body.innerText.includes('数据库 就绪')", "application startup");
await clickButton("短剧 Agent");
await waitFor("document.body.innerText.includes('引用小说章节')", "short drama source workspace");

let currentNovelRevisionId;
if (phase === "initial") {
  await clickButton("引用小说章节");
  await waitFor("document.body.innerText.includes('选择小说原文资产')", "novel source picker");
  await clickButton("新建小说");
  await setControl('input[aria-label="新小说名称"]', workTitle);
  await clickButton("创建并添加第一章");
  await waitFor("document.body.innerText.includes('新增 短剧来源验收小说 的章节')", "first chapter editor");
  await setControl('input[aria-label="章节名称"]', chapterTitle);
  await setControl('textarea[aria-label="小说章节正文"]', firstMarker);
  await clickButton("保存新章节");
  await waitFor("document.body.innerText.includes('引用此章节原文') && document.body.innerText.includes('雨夜来信')", "saved chapter preview");
  await clickButton("添加新章节");
  await waitFor("document.body.innerText.includes('新增 短剧来源验收小说 的章节')", "second chapter editor");
  await setControl('input[aria-label="章节名称"]', secondChapterTitle);
  await setControl('textarea[aria-label="小说章节正文"]', marker);
  await clickButton("保存新章节");
  await waitFor("document.body.innerText.includes('引用此章节原文') && document.body.innerText.includes('门外脚步')", "second chapter preview");
  await clickButton("引用此章节原文");
  await waitFor(`document.querySelector('textarea[aria-label="原始资料 Markdown"]')?.value === ${JSON.stringify(marker)}`, "selected source draft");
  await clickButton("保存初稿");
  await waitFor("document.body.innerText.includes('已保存') || document.body.innerText.includes('保存 v2')", "saved source document");
} else {
  await waitFor(`document.querySelector('textarea[aria-label="原始资料 Markdown"]')?.value === ${JSON.stringify(`# 原始资料\n\n## 类型\n小说章节\n\n## 正文\n${marker}`)}`, "restored source snapshot");
}

const source = await selectSourceRow();
if (!source) throw new Error("SOURCE_DOCUMENT_NOT_FOUND");
if (source.params.sourceKind !== "novel_chapter" || source.params.novelSourceReference?.content !== marker || source.params.novelSourceReference?.workTitle !== workTitle || source.params.novelSourceReference?.title !== secondChapterTitle || !source.params.novelChapterRevisionId) {
  throw new Error(`SOURCE_LINEAGE_MISMATCH:${JSON.stringify(source)}`);
}
if (source.text !== `# 原始资料\n\n## 类型\n小说章节\n\n## 正文\n${marker}`) {
  throw new Error(`SOURCE_SNAPSHOT_MISMATCH:${JSON.stringify(source.text)}`);
}
let chapters = await readChapterRows(source.params.novelWorkId, source.params.novelSourceReference.projectId);
if (!Array.isArray(chapters) || chapters.length !== 2 || Number(chapters[0].chapter_no) !== 1 || chapters[0].content !== firstMarker || Number(chapters[1].chapter_no) !== 2) {
  throw new Error(`CHAPTER_SQLITE_MISMATCH:${JSON.stringify(chapters)}`);
}

if (phase === "initial") {
  if (chapters[1].content !== marker || chapters[1].revision_id !== source.params.novelChapterRevisionId) throw new Error(`PRE_EDIT_CHAPTER_SNAPSHOT_MISMATCH:${JSON.stringify(chapters)}`);
  await clickButton("小说漫画");
  await waitFor("Boolean(document.querySelector('textarea[aria-label=\"小说原文预览\"]'))", "comic workspace source preview");
  await selectControl('select[aria-label="选择章节"]', source.params.novelChapterId);
  await waitFor(`document.querySelector('textarea[aria-label="小说原文预览"]')?.value === ${JSON.stringify(marker)}`, "comic uses selected second chapter");
  await clickButton("2. 作品设定");
  await waitFor("Boolean(document.querySelector('textarea[aria-label=\"作品设定 Markdown\"]'))", "comic settings editor");
  await setControl('textarea[aria-label="作品设定 Markdown"]', comicSettings);
  await clickButton("保存文字");
  await waitFor(`document.querySelector('textarea[aria-label="作品设定 Markdown"]')?.value === ${JSON.stringify(comicSettings)} && [...document.querySelectorAll('button')].some((item) => item.textContent?.trim() === '保存文字' && item.disabled)`, "manual comic settings saved");
  await clickButton("1. 原文资产");
  await waitFor(`document.querySelector('textarea[aria-label="小说原文预览"]')?.value === ${JSON.stringify(marker)}`, "return to comic source preview");
  const managerOpened = await evaluate(`(() => {
    const button = [...document.querySelectorAll('section[aria-label="小说原文资产"] button')].find((item) => item.textContent?.trim() === '管理小说原文');
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`);
  if (!managerOpened) throw new Error("COMIC_SHARED_MANAGER_UNAVAILABLE");
  await waitFor("document.body.innerText.includes('管理小说原文资产')", "comic shared manager");
  const managerWorkId = await evaluate("document.querySelector('select[aria-label=\"小说资产\"]')?.value");
  if (managerWorkId !== source.params.novelWorkId) throw new Error(`COMIC_MANAGER_WORK_MISMATCH:${JSON.stringify({ managerWorkId, expected: source.params.novelWorkId })}`);
  await waitFor("Boolean(document.querySelector('select[aria-label=\"小说章节\"]'))", "comic manager chapter list");
  const managerChapterId = await evaluate("document.querySelector('select[aria-label=\"小说章节\"]')?.value");
  if (managerChapterId !== source.params.novelChapterId) throw new Error(`COMIC_MANAGER_CHAPTER_MISMATCH:${JSON.stringify({ managerChapterId, expected: source.params.novelChapterId })}`);
  await clickButton("编辑并保存新修订");
  await waitFor("Boolean(document.querySelector('textarea[aria-label=\"小说章节正文\"]'))", "comic shared manager editor");
  await setControl('textarea[aria-label="小说章节正文"]', revisedMarker);
  await clickButton("保存新修订");
  await waitFor("document.body.innerText.includes('编辑并保存新修订') && !Boolean(document.querySelector('textarea[aria-label=\"小说章节正文\"]'))", "comic source revision saved");
  await clickButton("关闭");
  await waitFor(`document.querySelector('textarea[aria-label="小说原文预览"]')?.value === ${JSON.stringify(revisedMarker)}`, "comic refreshes its changed source");
  await clickButton("2. 作品设定");
  await waitFor(`document.querySelector('textarea[aria-label="作品设定 Markdown"]')?.value === ${JSON.stringify(comicSettings)}`, "comic adaptation document remains after source change");
  const originalSnapshotAfterComicEdit = await selectSourceRow();
  if (!originalSnapshotAfterComicEdit || originalSnapshotAfterComicEdit.params.novelChapterRevisionId !== source.params.novelChapterRevisionId || originalSnapshotAfterComicEdit.params.novelSourceReference?.content !== marker) {
    throw new Error(`SHORT_DRAMA_SNAPSHOT_OVERWRITTEN:${JSON.stringify(originalSnapshotAfterComicEdit)}`);
  }
  chapters = await readChapterRows(source.params.novelWorkId, source.params.novelSourceReference.projectId);
  currentNovelRevisionId = chapters.find((chapter) => Number(chapter.chapter_no) === 2)?.revision_id;
  if (!currentNovelRevisionId || currentNovelRevisionId === source.params.novelChapterRevisionId || chapters.find((chapter) => Number(chapter.chapter_no) === 2)?.content !== revisedMarker) {
    throw new Error(`COMIC_NEW_REVISION_NOT_PERSISTED:${JSON.stringify(chapters)}`);
  }
  await captureEvidence("comic-shared-manager-success");
} else {
  const initial = JSON.parse(await readFile(join(auditRoot, "short-drama-source-initial.json"), "utf8"));
  currentNovelRevisionId = chapters.find((chapter) => Number(chapter.chapter_no) === 2)?.revision_id;
  if (chapters.find((chapter) => Number(chapter.chapter_no) === 2)?.content !== revisedMarker || currentNovelRevisionId !== initial.currentNovelRevisionId) {
    throw new Error(`RESTART_NOVEL_REVISION_MISMATCH:${JSON.stringify({ initial, chapters })}`);
  }
}

await mkdir(auditRoot, { recursive: true });
await captureEvidence("success");
await writeFile(auditPath, JSON.stringify({ phase, sourceAssetId: source.id, revisionId: source.params.novelChapterRevisionId, currentNovelRevisionId, workId: source.params.novelWorkId, chapterId: source.params.novelChapterId, snapshot: source.params.novelSourceReference.content, chapters }, null, 2));
if (phase === "restore") {
  const initial = JSON.parse(await readFile(join(auditRoot, "short-drama-source-initial.json"), "utf8"));
  if (initial.revisionId !== source.params.novelChapterRevisionId || initial.currentNovelRevisionId !== currentNovelRevisionId || initial.snapshot !== source.params.novelSourceReference.content || initial.workId !== source.params.novelWorkId || initial.chapterId !== source.params.novelChapterId) {
    throw new Error(`RESTART_IDENTITY_MISMATCH:${JSON.stringify({ initial, restored: source })}`);
  }
}
console.log(JSON.stringify({ status: "SHORT_DRAMA_SOURCE_OK", phase, sourceAssetId: source.id, revisionId: source.params.novelChapterRevisionId }));
socket.close();
