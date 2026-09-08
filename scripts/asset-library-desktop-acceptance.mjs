import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join, resolve, sep } from "node:path";
import { DatabaseSync } from "node:sqlite";

const [cdpPort, auditRoot, phase] = process.argv.slice(2);
if (!cdpPort || !auditRoot || !["initial", "restore"].includes(phase)) throw new Error("Usage: node asset-library-desktop-acceptance.mjs <cdpPort> <auditRoot> <initial|restore>");
const auditPath = join(auditRoot, `asset-library-${phase}.json`);
const fixtureImageName = "external-upload-fixture.png";
const fixtureVideoName = "external-upload-fixture.mp4";
const onePrompt = "资产库验收：单条提示词导入后应完整替换生成页文本。";
const pageOne = "# 第1页\n\n## 画面要求\n雨夜街道，角色看向远处的灯。";
const pageTwo = "# 第2页\n\n## 画面要求\n镜头切到门前，信封落在水洼。";
const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

const hasPathKey = (value) => {
  if (Array.isArray(value)) return value.some(hasPathKey);
  if (!value || typeof value !== "object") return false;
  return Object.entries(value).some(([key, child]) => key.toLowerCase().includes("path") || hasPathKey(child));
};
const summarizePngDataUrl = (value) => {
  assert.equal(typeof value, "string", "provider image must be a string");
  const match = /^data:image\/png;base64,([A-Za-z0-9+/=]+)$/.exec(value);
  assert.ok(match, "native resolver must send a PNG data URL to the provider");
  const bytes = Buffer.from(match[1], "base64");
  assert.ok(bytes.subarray(0, pngSignature.length).equals(pngSignature), "native resolver must send decodable PNG bytes");
  return {
    mime: "image/png",
    bytes: bytes.length,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  };
};
const startLocalVideoProvider = async (videoBytes) => {
  const requests = [];
  let sequence = 0;
  const server = createServer(async (request, response) => {
    const url = new URL(request.url ?? "/", "http://127.0.0.1");
    const reply = (status, body, contentType = "application/json") => {
      response.writeHead(status, { "content-type": contentType, "content-length": Buffer.byteLength(body) });
      response.end(body);
    };
    try {
      if (request.method === "GET" && url.pathname === "/v1/models") {
        reply(200, JSON.stringify({ data: [{ id: "drama-video-v2" }, { id: "grok-imagine-video" }] }));
        return;
      }
      if (request.method === "POST" && url.pathname === "/v1/videos") {
        const chunks = [];
        for await (const chunk of request) chunks.push(chunk);
        const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
        assert.ok(!hasPathKey(body), "native provider request must never contain a path field");
        assert.ok(Array.isArray(body.images) && body.images.length === 1, "each isolated first-frame request must contain exactly one resolved image");
        const taskId = `local-video-${++sequence}`;
        requests.push({
          taskId,
          model: body.model,
          seconds: body.seconds,
          resolution: body.resolution,
          videoMode: body.video_mode,
          images: body.images.map(summarizePngDataUrl),
          hasPathKey: hasPathKey(body),
        });
        reply(200, JSON.stringify({ id: taskId }));
        return;
      }
      const task = /^\/v1\/videos\/(local-video-\d+)$/.exec(url.pathname);
      if (request.method === "GET" && task) {
        reply(200, JSON.stringify({ id: task[1], status: "completed" }));
        return;
      }
      const content = /^\/v1\/videos\/(local-video-\d+)\/content$/.exec(url.pathname);
      if (request.method === "GET" && content) {
        response.writeHead(200, { "content-type": "video/mp4", "content-length": videoBytes.length });
        response.end(videoBytes);
        return;
      }
      reply(404, JSON.stringify({ error: "fixture route not found" }));
    } catch (error) {
      reply(500, JSON.stringify({ error: error instanceof Error ? error.message : String(error) }));
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => { server.off("error", reject); resolve(); });
  });
  const address = server.address();
  assert.ok(address && typeof address === "object", "local video fixture server must bind a TCP port");
  return {
    requests,
    videoApiUrl: `http://127.0.0.1:${address.port}/v1/videos`,
    close: () => new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve())),
  };
};

const readCdpTargets = async () => {
  const deadline = Date.now() + 30_000;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`http://127.0.0.1:${cdpPort}/json`, { signal: AbortSignal.timeout(2_000) });
      if (!response.ok) throw new Error(`CDP_HTTP_${response.status}`);
      const targets = await response.json();
      if (Array.isArray(targets)) return targets;
      throw new Error("CDP_TARGETS_NOT_ARRAY");
    } catch (error) {
      lastError = error;
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
  }
  throw new Error(`CDP_TARGETS_TIMEOUT:${String(lastError)}`);
};
const targets = await readCdpTargets();
const page = targets.find((target) => target.type === "page");
if (!page?.webSocketDebuggerUrl) throw new Error("CDP_PAGE_UNAVAILABLE");
const socket = new WebSocket(page.webSocketDebuggerUrl);
let sequence = 0;
const pending = new Map();
const rejectPending = (reason) => {
  for (const { reject, timer } of pending.values()) {
    clearTimeout(timer);
    reject(reason);
  }
  pending.clear();
};
socket.addEventListener("message", (event) => {
  const message = JSON.parse(event.data);
  const request = pending.get(message.id);
  if (!request) return;
  pending.delete(message.id);
  clearTimeout(request.timer);
  request.resolve(message);
});
socket.addEventListener("error", () => rejectPending(new Error("CDP_SOCKET_ERROR")));
socket.addEventListener("close", () => rejectPending(new Error("CDP_SOCKET_CLOSED")));
await new Promise((resolve, reject) => {
  const timer = setTimeout(() => reject(new Error("CDP_SOCKET_OPEN_TIMEOUT")), 20_000);
  socket.addEventListener("open", () => { clearTimeout(timer); resolve(); }, { once: true });
  socket.addEventListener("error", () => { clearTimeout(timer); reject(new Error("CDP_SOCKET_ERROR")); }, { once: true });
  socket.addEventListener("close", () => { clearTimeout(timer); reject(new Error("CDP_SOCKET_CLOSED")); }, { once: true });
});
const send = (method, params = {}) => new Promise((resolve, reject) => {
  if (socket.readyState !== WebSocket.OPEN) { reject(new Error("CDP_SOCKET_NOT_OPEN")); return; }
  const id = ++sequence;
  const timer = setTimeout(() => {
    pending.delete(id);
    reject(new Error(`CDP_RPC_TIMEOUT:${method}`));
  }, 20_000);
  pending.set(id, { resolve, reject, timer });
  try { socket.send(JSON.stringify({ id, method, params })); } catch (error) { clearTimeout(timer); pending.delete(id); reject(error); }
});
const evaluate = async (expression) => { const response = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }); if (response.result?.exceptionDetails) throw new Error(response.result.exceptionDetails.exception?.description ?? response.result.exceptionDetails.text); return response.result?.result?.value; };
const invoke = (command, args = {}) => evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, ${JSON.stringify(args)})`);
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const checks = [];
const check = (name, evidence) => checks.push({ name, evidence });
const capture = async (label) => { await mkdir(auditRoot, { recursive: true }); const [text, shot] = await Promise.all([evaluate("document.body.innerText"), send("Page.captureScreenshot", { format: "png" })]); await writeFile(join(auditRoot, `asset-library-${phase}-${label}.txt`), String(text)); if (shot.result?.data) await writeFile(join(auditRoot, `asset-library-${phase}-${label}.png`), Buffer.from(shot.result.data, "base64")); };
const until = async (expression, label, timeout = 30000) => { const end = Date.now() + timeout; while (Date.now() < end) { if (await evaluate(expression)) return; await pause(150); } await capture(`timeout-${label}`); throw new Error(`Timed out waiting for ${label}`); };
const button = async (label) => { const clicked = await evaluate(`(() => { const item = [...document.querySelectorAll('button')].find((x) => x.textContent?.trim() === ${JSON.stringify(label)} || x.textContent?.includes(${JSON.stringify(label)})); if (!item || item.disabled) return false; item.click(); return true; })()`); if (!clicked) { await capture(`button-${label}`); throw new Error(`BUTTON_UNAVAILABLE:${label}`); } };
const exactButton = async (label) => { const clicked = await evaluate(`(() => { const item = [...document.querySelectorAll('button')].find((x) => x.textContent?.trim() === ${JSON.stringify(label)}); if (!item || item.disabled) return false; item.click(); return true; })()`); if (!clicked) throw new Error(`EXACT_BUTTON_UNAVAILABLE:${label}`); };
const ariaButton = async (label) => { const clicked = await evaluate(`(() => { const item = document.querySelector('button[aria-label=' + JSON.stringify(${JSON.stringify(label)}) + ']'); if (!(item instanceof HTMLButtonElement) || item.disabled) return false; item.click(); return true; })()`); if (!clicked) throw new Error(`ARIA_BUTTON_UNAVAILABLE:${label}`); };
const clickNav = async (label) => { await exactButton(label); await pause(250); };
const waitForAssetLibraryControls = () => until("[...document.querySelectorAll('button')].some((item) => item.textContent?.trim() === '刷新全部')", "asset library controls");
const clickCatalogTab = async (label) => {
  const clicked = await evaluate(`(() => {
    const normalizedLabel = ${JSON.stringify(label)}.replace(/\\s/g, '');
    const matches = [...document.querySelectorAll('button')].filter((item) => {
      const count = [...item.querySelectorAll('span')].map((span) => span.textContent?.trim() ?? '').find((value) => /^\\d+$/.test(value));
      return Boolean(count) && (item.textContent ?? '').replace(/\\s/g, '') === normalizedLabel + count;
    });
    if (matches.length !== 1 || matches[0].disabled) return { ok: false, matches: matches.length };
    matches[0].click();
    return { ok: true, matches: 1 };
  })()`);
  if (!clicked?.ok) { await capture(`catalog-tab-${label}`); throw new Error(`CATALOG_TAB_UNAVAILABLE:${label}:${clicked?.matches ?? 0}`); }
  await pause(180);
};
const assertCatalogResults = async (label, { present, absent }) => {
  await clickCatalogTab(label);
  await until(present.map((value) => `document.body.innerText.includes(${JSON.stringify(value)})`).join(" && "), `catalog-${label}-present`);
  const result = await evaluate("document.body.innerText");
  for (const value of present) assert.ok(result.includes(value), `${label} catalog must include ${value}`);
  for (const value of absent) assert.ok(!result.includes(value), `${label} catalog must exclude unrelated ${value}`);
  check(`catalog tab filters fixtures: ${label}`, { present, absent });
};
const clickPickerEntry = async (label) => {
  const clicked = await evaluate(`(() => {
    const dialog = document.querySelector('[role=dialog][aria-label="导入当前项目的提示词或实际模型参考"]');
    const item = [...(dialog?.querySelectorAll('button') ?? [])].find((button) => button.textContent?.includes(${JSON.stringify(label)}));
    if (!item || item.disabled) return false;
    item.click();
    return true;
  })()`);
  if (!clicked) throw new Error(`PICKER_ENTRY_UNAVAILABLE:${label}`);
};
const recordedNativeCommands = async () => {
  const logDir = join(auditRoot, "data", "logs");
  const logFiles = (await readdir(logDir)).filter((name) => name.endsWith(".log"));
  const logText = (await Promise.all(logFiles.map((name) => readFile(join(logDir, name), "utf8")))).join("\n");
  return [...logText.matchAll(/"command":"([^"]+)"/g)].map((match) => match[1]);
};
const setValue = async (selector, value) => { const result = await evaluate(`(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!(e instanceof HTMLInputElement || e instanceof HTMLTextAreaElement)) return false; Object.getOwnPropertyDescriptor(Object.getPrototypeOf(e), 'value').set.call(e, ${JSON.stringify(value)}); e.dispatchEvent(new Event('input', {bubbles:true})); e.dispatchEvent(new Event('change', {bubbles:true})); return true; })()`); if (!result) throw new Error(`CONTROL_UNAVAILABLE:${selector}`); };
const selectValue = async (selector, value) => { const result = await evaluate(`(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!(e instanceof HTMLSelectElement) || ![...e.options].some((o) => o.value === ${JSON.stringify(value)})) return false; Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(e, ${JSON.stringify(value)}); e.dispatchEvent(new Event('change', {bubbles:true})); return true; })()`); if (!result) throw new Error(`SELECT_UNAVAILABLE:${selector}`); };
const selectByOption = async (value) => { const result = await evaluate(`(() => { const matches = [...document.querySelectorAll('select')].filter((e) => [...e.options].some((o) => o.value === ${JSON.stringify(value)})); if (matches.length !== 1) return { ok: false, matches: matches.length }; const e = matches[0]; Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(e, ${JSON.stringify(value)}); e.dispatchEvent(new Event('change', {bubbles:true})); return { ok: true, matches: 1 }; })()`); if (!result?.ok) throw new Error(`SELECT_OPTION_UNAVAILABLE:${value}:${result?.matches ?? 0}`); };
const clickCardImageImport = async (label) => { const clicked = await evaluate(`(() => { const card = [...document.querySelectorAll('article')].find((x) => x.textContent?.includes(${JSON.stringify(label)})); const item = [...(card?.querySelectorAll('button') ?? [])].find((x) => x.textContent?.trim() === '用于生图'); if (!item || item.disabled) return false; item.click(); return true; })()`); if (!clicked) throw new Error(`CARD_IMAGE_IMPORT_UNAVAILABLE:${label}`); };
const clickComicGroupImageImport = async () => { const clicked = await evaluate(`(() => { const heading = [...document.querySelectorAll('h3')].find((x) => x.textContent?.includes('第1章')); const section = heading?.parentElement?.parentElement; const item = [...(section?.querySelectorAll('button') ?? [])].find((x) => x.textContent?.trim() === '用于生图'); if (!item || item.disabled) return false; item.click(); return true; })()`); if (!clicked) throw new Error('COMIC_GROUP_IMAGE_IMPORT_UNAVAILABLE'); };
const assetRows = () => invoke("db_select", { query: "SELECT id,kind,path,metadata FROM assets ORDER BY created_at ASC", bindValues: [] });
const insertCanonicalImageFixture = async ({ projectId, workId, chapterId, documentId, documentRevision, imagePath, jobId, imageId }) => {
  const isolatedDataRoot = resolve(auditRoot, "data");
  const databasePath = resolve(isolatedDataRoot, "image-client.db");
  assert.ok(databasePath.startsWith(`${isolatedDataRoot}${sep}`), "canonical image fixture database must stay below this run's isolated data directory");
  await readFile(databasePath);
  const database = new DatabaseSync(databasePath, { enableForeignKeyConstraints: true, timeout: 5_000 });
  try {
    database.exec("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; BEGIN IMMEDIATE;");
    const page = database.prepare("SELECT 1 FROM comic_md_revisions WHERE document_id=? AND revision=?").get(documentId, documentRevision);
    const chapter = database.prepare("SELECT 1 FROM novel_chapters WHERE id=? AND novel_work_id=?").get(chapterId, workId);
    assert.ok(page && chapter, "canonical image fixture must reference existing isolated canonical rows");
    database.prepare("INSERT INTO comic_md_jobs(id,project_id,novel_work_id,chapter_id,kind,status,input_snapshot,completed_pages,total_pages,created_at) VALUES(?,?,?,?,?,'succeeded',?,1,1,?)")
      .run(jobId, projectId, workId, chapterId, "images", JSON.stringify({ fixture: true, source: "offline desktop acceptance" }), Date.now());
    database.prepare("INSERT INTO comic_md_images(id,job_id,document_id,document_revision,page_no,path,created_at,effective_prompt) VALUES(?,?,?,?,?,?,?,?)")
      .run(imageId, jobId, documentId, documentRevision, 1, imagePath, Date.now(), "离线 canonical 图片夹具的完整生图提示词");
    database.exec("COMMIT;");
  } catch (error) {
    try { database.exec("ROLLBACK;"); } catch { /* transaction was not opened */ }
    throw error;
  } finally {
    database.close();
  }
};
const writeFixtureAsset = async ({ projectId, imageBytes, videoBytes, name, kind, category, origin, group }) => {
  const imported = await invoke("asset_import_files", { input: { projectId, importEntry: "asset_library_upload", uploadBatchId: `fixture-${category}-${kind}`, files: [{ source: "bytes", fileName: name, data: [...(kind === "image" ? imageBytes : videoBytes)] }], params: { fixture: true } } });
  assert.equal(imported.length, 1);
  const meta = { source: name, projectId, fixture: true, params: { fixture: true, fixturePurpose: "ordinary-generation-preview-only", prompt: kind === "image" ? "普通生图夹具提示词" : "普通视频夹具提示词", catalog: { version: 1, category, origin, group } } };
  await invoke("db_execute", { query: "UPDATE assets SET metadata=? WHERE id=?", bindValues: [JSON.stringify(meta), imported[0].asset.id] });
  return { ...imported[0], params: meta.params };
};

try {
  await until("Boolean(window.__TAURI_INTERNALS__) && document.body.innerText.includes('数据库 就绪')", "desktop startup");
  if (phase === "initial") {
    const [imageBytes, videoBytes] = await Promise.all([readFile(join(auditRoot, "fixtures", fixtureImageName)), readFile(join(auditRoot, "fixtures", fixtureVideoName))]);
    const activeRows = await invoke("db_select", { query: "SELECT value FROM settings WHERE key=?", bindValues: ["active_project"] });
    const projectId = activeRows[0]?.value;
    assert.ok(projectId, "isolated desktop must initialize a project");
    const projectsRows = await invoke("db_select", { query: "SELECT value FROM settings WHERE key=?", bindValues: ["projects"] });
    const projects = JSON.parse(projectsRows[0].value);
    const projectTwo = { ...projects[0], id: "asset-library-second-project", name: "资产库隔离第二项目" };
    await invoke("db_execute", { query: "UPDATE settings SET value=? WHERE key='projects'", bindValues: [JSON.stringify([...projects, projectTwo])] });
    await evaluate("location.reload(); true");
    await until("Boolean(window.__TAURI_INTERNALS__) && document.body.innerText.includes('数据库 就绪')", "project fixture reload");
    const normalImage = await writeFixtureAsset({ projectId, imageBytes, videoBytes, name: "ordinary-image-fixture.png", kind: "image", category: "image_generation", origin: "provider", group: { type: "generation_batch", id: "fixture-image-batch" } });
    const normalVideo = await writeFixtureAsset({ projectId, imageBytes, videoBytes, name: "ordinary-video-fixture.mp4", kind: "video", category: "video_generation", origin: "provider", group: { type: "video_shots", id: "fixture-video-shots" } });
    const promptAsset = await writeFixtureAsset({ projectId, imageBytes, videoBytes, name: "short-drama-prompt-fixture.txt", kind: "image", category: "short_drama", origin: "workspace", group: { type: "short_drama_workflow", id: "fixture-short-drama" } });
    await invoke("db_execute", { query: "UPDATE assets SET kind='text', path='', metadata=? WHERE id=?", bindValues: [JSON.stringify({ source: "短剧单提示词夹具", projectId, fixture: true, params: { fixture: true, prompt: onePrompt, catalog: { version: 1, category: "short_drama", origin: "workspace", group: { type: "short_drama_workflow", id: "fixture-short-drama" } } } }), promptAsset.asset.id] });
    const uploads = await invoke("asset_import_files", { input: { projectId, importEntry: "asset_library_upload", uploadBatchId: "desktop-upload-batch", files: [{ source: "bytes", fileName: fixtureImageName, data: [...imageBytes] }, { source: "bytes", fileName: fixtureVideoName, data: [...videoBytes] }], params: { acceptance: "native-cdp-json-bytes" } } });
    assert.equal(uploads.length, 2, "native bytes contract must atomically import both selected files");
    assert.deepEqual(uploads.map((item) => item.params.originalFileName).sort(), [fixtureImageName, fixtureVideoName].sort());
    assert.deepEqual(uploads.map((item) => item.asset.kind).sort(), ["image", "video"]);
    const work = await invoke("novel_work_create", { input: { projectId, title: "资产库验收小说", description: "isolated fixture", idempotencyKey: "asset-library-work" } });
    const chapter = await invoke("novel_chapter_revision_create", { input: { projectId, novelWorkId: work.id, chapterNo: 1, title: "雨夜两页", content: "雨夜里，主角拿起一封没有署名的信。", sourceKind: "paste", idempotencyKey: "asset-library-chapter" } });
    const scope = { projectId, novelWorkId: work.id, chapterId: chapter.chapterId };
    const settings = await invoke("comic_md_document_save", { input: { ...scope, kind: "settings", markdown: "# 作品设定\n\n## 世界观\n雨夜小城\n\n## 画风\n水墨\n\n## 人物锚点\n主角穿深色风衣", expectedRevision: null } });
    const p1 = await invoke("comic_md_document_save", { input: { ...scope, kind: "page_prompt", pageNo: 1, markdown: pageOne, expectedRevision: null } });
    const p2 = await invoke("comic_md_document_save", { input: { ...scope, kind: "page_prompt", pageNo: 2, markdown: pageTwo, expectedRevision: null } });
    assert.ok(settings.id && p1.id && p2.id);
    const canonicalImageJobId = "asset-library-canonical-image-job";
    const canonicalImageId = "asset-library-canonical-image";
    await insertCanonicalImageFixture({ projectId, workId: work.id, chapterId: chapter.chapterId, documentId: p1.id, documentRevision: p1.revision, imagePath: join(auditRoot, "fixtures", fixtureImageName), jobId: canonicalImageJobId, imageId: canonicalImageId });
    const catalog = await invoke("comic_md_catalog_list", { input: { projectId } });
    assert.equal(catalog.filter((entry) => entry.kind === "text" && entry.documentKind === "page_prompt").length, 2, "canonical comic pages must be available through read-only catalog IPC");
    const canonicalImage = catalog.find((entry) => entry.kind === "image" && entry.sourceUri?.endsWith(`/${canonicalImageId}`));
    assert.deepEqual(canonicalImage && { path: canonicalImage.path, effectivePrompt: canonicalImage.effectivePrompt, promptSnapshotComplete: canonicalImage.promptSnapshotComplete }, { path: join(auditRoot, "fixtures", fixtureImageName), effectivePrompt: "离线 canonical 图片夹具的完整生图提示词", promptSnapshotComplete: true }, "offline canonical image fixture must use the read-only comic catalog schema and retain its effective prompt");
    const allRows = await assetRows();
    check("native fixture setup", { projectId, ordinaryImageId: normalImage.asset.id, ordinaryVideoId: normalVideo.asset.id, uploadKinds: uploads.map((item) => item.asset.kind), canonicalComicRows: catalog.length, canonicalImageId, allAssetRows: allRows.length });
    const localProvider = await startLocalVideoProvider(videoBytes);
    try {
      const status = await invoke("save_config", { config: {
        imageApiUrl: "", imageApiKey: "", imageApiModel: "",
        videoApiUrl: localProvider.videoApiUrl, videoApiKey: "isolated-local-fixture-key", videoApiModel: "grok-imagine-video",
        llmApiUrl: "", llmApiKey: "", llmApiModel: "", outputDir: "asset-library-local-video-fixture",
      } });
      assert.equal(status.videoReady, true, "isolated loopback provider must be the active video configuration");
      const runLocalFirstFrame = async (localImage) => invoke("run_video", { req: {
        nodeType: "video_generation",
        category: "video",
        inputAssets: [],
        config: {
          project_id: projectId,
          model: "grok-imagine-video",
          prompt: "保持输入图的构图，镜头轻微推进。",
          duration_s: 1,
          resolution: "480p",
          mode: "first_frame",
          images: [],
          local_images: [localImage],
          videos: [],
          audios: [],
        },
      } });
      const [ordinaryRun, canonicalRun] = await Promise.all([
        runLocalFirstFrame({ assetId: normalImage.asset.id }),
        runLocalFirstFrame({ sourceUri: canonicalImage.sourceUri }),
      ]);
      const generatedAssets = [...ordinaryRun.assets, ...canonicalRun.assets];
      assert.equal(generatedAssets.length, 2, "both local identities must complete the native video pipeline");
      const generatedBytes = await Promise.all(generatedAssets.map((asset) => readFile(asset.path)));
      assert.ok(generatedBytes.every((bytes) => bytes.equals(videoBytes)), "native pipeline must persist the fixture provider MP4 only after completed polling");
      assert.equal(localProvider.requests.length, 2, "provider must receive one request for the ordinary asset and one for the canonical source URI");
      assert.ok(localProvider.requests.every((request) => request.model === "grok-imagine-video" && request.seconds === "1" && request.resolution === "480p" && request.videoMode === "first_frame" && request.images.length === 1 && request.images[0].mime === "image/png" && request.images[0].bytes > pngSignature.length && !request.hasPathKey), "native provider boundary must contain only one verified PNG data URI and no client path");
      check("native local image identities resolve through isolated provider to completed MP4", {
        provider: "127.0.0.1 loopback",
        requestCount: localProvider.requests.length,
        requestBodies: localProvider.requests,
        inputIdentities: ["ordinary assetId", "canonical comic sourceUri"],
        generatedMp4Sha256: generatedBytes.map((bytes) => createHash("sha256").update(bytes).digest("hex")),
      });
    } finally {
      await localProvider.close();
    }
    await clickNav("资产库");
    await until("[...document.querySelectorAll('button')].some((item) => item.textContent?.trim() === '刷新全部')", "asset library controls");
    await exactButton("刷新全部");
    await assertCatalogResults("全部", {
      present: ["ordinary-image-fixture.png", "ordinary-video-fixture.mp4", "资产库验收小说", "第1页", "第2页", "漫画第1页", "短剧单提示词夹具", fixtureImageName, fixtureVideoName],
      absent: [],
    });
    const previewState = await evaluate(`Promise.all([...document.querySelectorAll('img,video')].map((node) => node instanceof HTMLImageElement ? new Promise((resolve) => { if (node.complete) resolve({tag:'img', ok:node.naturalWidth > 0}); else { node.addEventListener('load', () => resolve({tag:'img',ok:true}), {once:true}); node.addEventListener('error', () => resolve({tag:'img',ok:false}), {once:true}); } }) : new Promise((resolve) => { if (node.readyState >= 2) resolve({tag:'video',ok:true}); else { node.addEventListener('loadeddata', () => resolve({tag:'video',ok:true}), {once:true}); node.addEventListener('error', () => resolve({tag:'video',ok:false}), {once:true}); setTimeout(() => resolve({tag:'video',ok:node.readyState >= 2}), 3000); } })))`);
    assert.ok(previewState.some((entry) => entry.tag === "img" && entry.ok), "an actual image preview must decode");
    assert.ok(previewState.some((entry) => entry.tag === "video" && entry.ok), "an actual video preview must load metadata/data");
    check("all tab contains ordinary, canonical text/image, novel and short-drama records with decoded image/video preview", { previewState, requiredText: ["ordinary-image-fixture.png", "ordinary-video-fixture.mp4", "资产库验收小说", "第1页", "第2页", "漫画第1页", "短剧单提示词夹具"] });
    await assertCatalogResults("生图", { present: ["ordinary-image-fixture.png"], absent: ["ordinary-video-fixture.mp4", "资产库验收小说", "第1页", "短剧单提示词夹具", fixtureImageName] });
    await assertCatalogResults("视频", { present: ["ordinary-video-fixture.mp4"], absent: ["ordinary-image-fixture.png", "资产库验收小说", "第1页", "短剧单提示词夹具", fixtureVideoName] });
    await assertCatalogResults("小说", { present: ["资产库验收小说"], absent: ["ordinary-image-fixture.png", "ordinary-video-fixture.mp4", "第1页", "短剧单提示词夹具", fixtureImageName] });
    await assertCatalogResults("漫画", { present: ["第1页", "第2页", "漫画第1页"], absent: ["ordinary-image-fixture.png", "ordinary-video-fixture.mp4", "短剧单提示词夹具", fixtureImageName] });
    await assertCatalogResults("短剧", { present: ["短剧单提示词夹具"], absent: ["ordinary-image-fixture.png", "ordinary-video-fixture.mp4", "资产库验收小说", "第1页", fixtureImageName] });
    await assertCatalogResults("外部上传", { present: [fixtureImageName, fixtureVideoName], absent: ["ordinary-image-fixture.png", "ordinary-video-fixture.mp4", "资产库验收小说", "第1页", "短剧单提示词夹具"] });
    await clickCatalogTab("全部");
    await exactButton("使用文档");
    await until("[...document.querySelectorAll('[role=dialog] h2')].some((item) => item.textContent?.trim() === '使用文档')", "application usage guide");
    const guideText = await evaluate("document.querySelector('[role=dialog]')?.innerText ?? ''");
    for (const required of ["本地图片", "媒体托管", "共享小说原文管理器", "章节"]) assert.ok(guideText.includes(required), `usage guide must document ${required}`);
    check("in-app usage guide documents local/hosted images and shared novel chapters", { required: ["本地图片", "媒体托管", "共享小说原文管理器", "章节"] });
    await capture("usage-guide-v023");
    await ariaButton("关闭使用文档");
    await exactButton("更新日志");
    await until("[...document.querySelectorAll('[role=dialog] h2')].some((item) => item.textContent?.trim() === '更新日志')", "application changelog");
    const changelogText = await evaluate("document.querySelector('[role=dialog]')?.innerText ?? ''");
    for (const required of ["v0.2.3", "统一资产与引用", "漫画可核对性"]) assert.ok(changelogText.includes(required), `changelog must include ${required}`);
    check("in-app v0.2.3 changelog is present", { required: ["v0.2.3", "统一资产与引用", "漫画可核对性"] });
    await capture("changelog-v023");
    await ariaButton("关闭更新日志");
    const hostingSettings = await invoke("db_select", { query: "SELECT value FROM settings WHERE key='media_hosting.v1'", bindValues: [] });
    assert.equal(hostingSettings.length, 0, "isolated desktop fixture must begin without a media-hosting configuration");
    await clickNav("视频生成");
    await until("[...document.querySelectorAll('button')].some((item) => item.textContent?.trim() === '从资产库导入')", "video workspace import control");
    await until("[...document.querySelectorAll('select')].some((select) => [...select.options].some((option) => option.value === 'grok-imagine-video'))", "Grok single-image reference model option");
    await selectByOption("grok-imagine-video");
    await until("document.body.innerText.includes('Grok Imagine Video')", "Grok single-image reference model");
    await exactButton("从资产库导入");
    await until("Boolean(document.querySelector('[role=dialog][aria-label=\"导入当前项目的提示词或实际模型参考\"]'))", "video import picker");
    await exactButton("参考资源");
    await clickPickerEntry("ordinary-image-fixture.png");
    await until("document.querySelector('[aria-label=导入预览]')?.innerText.includes('ordinary-image-fixture.png')", "local video reference preview");
    const commandsBeforeDirectLocal = await recordedNativeCommands();
    await exactButton("确认应用");
    await until("document.body.innerText.includes('本地图片 · ordinary-image-fixture.png')", "direct local video image applied");
    const commandsAfterDirectLocal = await recordedNativeCommands();
    assert.ok(!commandsAfterDirectLocal.slice(commandsBeforeDirectLocal.length).includes("asset_publish_media"), "direct local image reference must not publish media");
    check("local image applies directly to video reference without media hosting", { commandDelta: commandsAfterDirectLocal.slice(commandsBeforeDirectLocal.length) });
    await setValue("textarea", "保持输入图构图，镜头轻微推进。");
    const grokReady = await evaluate(`(() => {
      const generate = [...document.querySelectorAll('button')].find((item) => item.textContent?.trim() === '生成 1 个视频镜头');
      return {
        enabled: Boolean(generate && !generate.disabled),
        firstFrame: document.body.innerText.includes('首帧驱动'),
        localImage: document.body.innerText.includes('本地图片 · ordinary-image-fixture.png'),
      };
    })()`);
    assert.deepEqual(grokReady, { enabled: true, firstFrame: true, localImage: true }, "one local image must make Grok first-frame generation ready without requiring seven reference images");
    check("Grok one-local-image first-frame UI is enabled without the seven-image reference requirement", grokReady);
    await capture("local-reference-ready");
    await exactButton("从资产库导入");
    await until("Boolean(document.querySelector('[role=dialog][aria-label=\"导入当前项目的提示词或实际模型参考\"]'))", "video import picker for explicit hosting");
    await exactButton("参考资源");
    await clickPickerEntry("ordinary-image-fixture.png");
    await until("document.querySelector('[aria-label=导入预览]')?.innerText.includes('ordinary-image-fixture.png')", "explicit hosting local video reference preview");
    await exactButton("经托管转为 URL");
    await exactButton("上传并引用");
    await until("Boolean(document.querySelector('[role=dialog][aria-label=\"媒体托管配置\"]'))", "missing media hosting dialog");
    await until("document.body.innerText.includes('请先配置媒体托管服务')", "missing media hosting explanation");
    const hostingCommands = await recordedNativeCommands();
    assert.ok(hostingCommands.includes("media_hosting_get"), "local-reference UI must inspect hosting configuration before publishing");
    assert.ok(!hostingCommands.includes("asset_publish_media"), "unconfigured hosting must not call the only native command that can make a media-hosting request");
    check("unconfigured media-hosting UI opens configuration and performs no publish request", { hostingCommands });
    await capture("hosting-unconfigured");
    await ariaButton("关闭媒体托管配置");
    await exactButton("取消");
    await clickNav("资产库");
    await until("document.body.innerText.includes('短剧单提示词夹具')", "asset library after hosting cancellation");
    await clickCardImageImport("短剧单提示词夹具");
    await until("Boolean(document.querySelector('[role=dialog][aria-label=\"从资产库导入\"]'))", "single prompt picker");
    await until(`document.querySelector('[aria-label="导入预览"]')?.innerText.includes(${JSON.stringify(onePrompt)})`, "single prompt preview");
    await exactButton("确认应用");
    await until(`document.querySelector('textarea')?.value.includes(${JSON.stringify(onePrompt)})`, "single prompt applied");
    check("single prompt import applies only after confirmation", { prompt: onePrompt });
    await clickNav("资产库");
    await until("document.body.innerText.includes('第1页') && document.body.innerText.includes('第2页')", "two comic page cards");
    await clickComicGroupImageImport();
    await until("Boolean(document.querySelector('[aria-label=导入预览]'))", "comic group import preview");
    const mergedPreview = await evaluate("document.querySelector('[aria-label=导入预览]')?.innerText");
    assert.ok(mergedPreview.indexOf("第1页") < mergedPreview.indexOf("第2页"), "comic import preview must retain page order");
    const beforeCancel = await evaluate("document.querySelector('textarea')?.value");
    await exactButton("取消");
    assert.equal(await evaluate("document.querySelector('textarea')?.value"), beforeCancel, "cancel must leave generation form untouched");
    await clickNav("资产库"); await clickComicGroupImageImport(); await until("Boolean(document.querySelector('[aria-label=导入预览]'))", "comic merge reopen"); await exactButton("确认应用");
    await until(`document.querySelector('textarea')?.value.includes(${JSON.stringify(pageOne)}) && document.querySelector('textarea')?.value.includes(${JSON.stringify(pageTwo)})`, "comic merge confirmed");
    check("two canonical comic pages preserve preview order; cancel leaves form unchanged; confirmation merges both", { preview: mergedPreview });
    await exactButton("从已有资产选择或合并参考");
    await until("Boolean(document.querySelector('[role=dialog][aria-label=\"从资产库导入\"]'))", "reference picker");
    await exactButton("参考资源");
    await until("[...document.querySelectorAll('[role=dialog][aria-label=\"从资产库导入\"] button')].some((item) => item.textContent?.includes('漫画第1页'))", "canonical image available to reference picker");
    const candidates = await evaluate(`(() => [...document.querySelectorAll('[role=dialog] button')].filter((x) => x.textContent?.includes('ordinary-image-fixture') || x.textContent?.includes('漫画第1页')).map((x) => x.textContent))()`);
    assert.ok(candidates.some((value) => value?.includes("ordinary-image-fixture")) && candidates.some((value) => value?.includes("漫画第1页")), "an ordinary image and a read-only canonical comic image must be selectable for multi-reference import");
    const referencesSelected = await evaluate(`(() => { const dialog = document.querySelector('[role=dialog][aria-label="从资产库导入"]'); const ordinary = [...(dialog?.querySelectorAll('button') ?? [])].find((x) => x.textContent?.includes('ordinary-image-fixture')); const canonical = [...(dialog?.querySelectorAll('button') ?? [])].find((x) => x.textContent?.includes('漫画第1页')); if (!ordinary || !canonical) return false; ordinary.click(); canonical.click(); return true; })()`);
    assert.ok(referencesSelected, "reference picker must select both ordinary and canonical image buttons");
    await exactButton("确认应用");
    await until("document.body.innerText.includes('2 项参考资源') && document.querySelectorAll('[aria-label=已选参考图] img').length === 2", "multi-image references applied");
    check("ordinary and read-only canonical comic images selected as two references through UI confirmation", { candidates, canonicalImageId });
    await clickNav("资产库");
    await selectByOption("asset-library-second-project");
    await until("document.body.innerText.includes('资产库隔离第二项目')", "second project selected");
    await clickNav("图像生成");
    await until("document.querySelectorAll('[aria-label=已选参考图] img').length === 0 && !document.body.innerText.includes('2 项参考资源')", "second project image reference state cleared");
    const afterSwitch = await evaluate(`({ text: document.body.innerText, referencePreviews: document.querySelectorAll('[aria-label=已选参考图] img').length, pendingDialog: Boolean(document.querySelector('[role=dialog][aria-label*="导入"]')) })`);
    assert.equal(afterSwitch.referencePreviews, 0, "project switch must clear the image-generation reference previews");
    assert.ok(!afterSwitch.pendingDialog, "project switch must clear the import queue UI");
    check("project switch clears image-generation references and queue", afterSwitch);
    await clickNav("资产库"); await waitForAssetLibraryControls();
    await selectByOption(projectId); await until("document.body.innerText.includes('默认项目') || document.body.innerText.includes('资产库')", "original project restored");
    const checkpoint = { projectId, workId: work.id, chapterId: chapter.chapterId, uploadIds: uploads.map((item) => item.asset.id), expectedUploadNames: [fixtureImageName, fixtureVideoName], checks };
    await capture("initial-success"); await writeFile(auditPath, JSON.stringify(checkpoint, null, 2));
  } else {
    const checkpoint = JSON.parse(await readFile(join(auditRoot, "asset-library-initial.json"), "utf8"));
    const rows = await assetRows();
    const restored = rows.filter((row) => checkpoint.uploadIds.includes(row.id)).map((row) => ({ id: row.id, kind: row.kind, metadata: JSON.parse(row.metadata) }));
    assert.equal(restored.length, 2, "both uploaded rows must survive restart");
    assert.deepEqual(restored.map((row) => row.metadata.params.originalFileName).sort(), checkpoint.expectedUploadNames.sort());
    assert.ok(restored.every((row) => row.metadata.params.catalog?.category === "upload" && row.metadata.params.uploadBatchId === "desktop-upload-batch"));
    await clickNav("资产库"); await waitForAssetLibraryControls(); await clickCatalogTab("外部上传");
    await until(`document.body.innerText.includes(${JSON.stringify(fixtureImageName)}) && document.body.innerText.includes(${JSON.stringify(fixtureVideoName)})`, "restored upload tab");
    const restorePreview = await evaluate("[...document.querySelectorAll('img,video')].map((node) => node.tagName + ':' + (node instanceof HTMLImageElement ? node.complete && node.naturalWidth > 0 : node.readyState >= 1))");
    assert.ok(restorePreview.some((value) => value === "IMG:true")); assert.ok(restorePreview.some((value) => value === "VIDEO:true"));
    check("restart retains upload records, JSON provenance and media previews", { restored, restorePreview });
    await capture("restore-success"); await writeFile(auditPath, JSON.stringify({ ...checkpoint, restoreChecks: checks }, null, 2));
  }
  console.log(JSON.stringify({ status: "ASSET_LIBRARY_DESKTOP_OK", phase, auditPath }));
} catch (error) {
  try { await capture("failure"); } catch { /* retain original failure */ }
  throw error;
} finally { socket.close(); }
