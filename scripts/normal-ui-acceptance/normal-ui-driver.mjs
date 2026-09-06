#!/usr/bin/env node
// Inert unless --run is supplied after the visible normal supervisor starts.
// It never invokes Tauri, Zustand/store internals, providers, or retries.
import { createHash, randomBytes } from "node:crypto";
import { existsSync, readFileSync, writeFileSync, openSync, closeSync, fsyncSync, renameSync, mkdirSync, lstatSync } from "node:fs";
import { basename, dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync, spawnSync } from "node:child_process";

const fail = (code) => { throw new Error(code); };
function parseArgs(argv) {
  const flags = new Set(["--run", "--self-test"]);
  const values = new Set(["--context", "--uia-helper", "--uia-helper-sha256"]);
  const parsed = {};
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (flags.has(argument)) {
      if (parsed[argument] !== undefined) fail("NORMAL_UI_ARGUMENT_DUPLICATE:" + argument);
      parsed[argument] = true;
      continue;
    }
    if (values.has(argument)) {
      if (parsed[argument] !== undefined || index + 1 >= argv.length || argv[index + 1].startsWith("--")) fail("NORMAL_UI_ARGUMENT_VALUE_INVALID:" + argument);
      parsed[argument] = argv[index + 1]; index += 1;
      continue;
    }
    fail("NORMAL_UI_ARGUMENT_UNKNOWN:" + argument);
  }
  if (parsed["--run"] && parsed["--self-test"]) fail("NORMAL_UI_ARGUMENT_MODE_CONFLICT");
  if (parsed["--self-test"] && Object.keys(parsed).length !== 1) fail("NORMAL_UI_ARGUMENT_SELF_TEST_ONLY");
  if (!parsed["--self-test"] && typeof parsed["--context"] !== "string") fail("NORMAL_UI_CONTEXT_MISSING");
  if (typeof parsed["--uia-helper"] === "string" && typeof parsed["--uia-helper-sha256"] !== "string") fail("NORMAL_UI_UIA_OVERRIDE_HASH_REQUIRED");
  if (typeof parsed["--uia-helper"] !== "string" && typeof parsed["--uia-helper-sha256"] === "string") fail("NORMAL_UI_UIA_OVERRIDE_PATH_REQUIRED");
  return parsed;
}
const parsedArgs = parseArgs(process.argv.slice(2));
const run = parsedArgs["--run"] === true;
const selfTest = parsedArgs["--self-test"] === true;
const contextPath = parsedArgs["--context"];
const uiaHelperOverride = parsedArgs["--uia-helper"];
const uiaHelperOverrideSha256 = parsedArgs["--uia-helper-sha256"];
let context;
let cdpPort;
let normalPid;
let stopSupervisor;
let completed = false;

function assertDChild(root, value, code) {
  const absoluteRoot = resolve(root), absoluteValue = resolve(value), relation = relative(absoluteRoot, absoluteValue);
  if (!/^D:[\\/]/i.test(absoluteRoot) || !/^D:[\\/]/i.test(absoluteValue)) fail(code + "_NOT_D");
  if (relation === ".." || relation.startsWith("..\\") || relation.startsWith("../")) fail(code + "_OUTSIDE_RUNTIME");
  for (let cursor = absoluteValue;; cursor = dirname(cursor)) {
    if (existsSync(cursor) && lstatSync(cursor).isSymbolicLink()) fail(code + "_REPARSE_ANCESTOR");
    const parent = dirname(cursor); if (parent === cursor) break;
  }
  return absoluteValue;
}
function readContext(path) {
  if (typeof path !== "string" || !existsSync(path)) fail("NORMAL_UI_CONTEXT_MISSING");
  const context = JSON.parse(readFileSync(path, "utf8"));
  if (context?.schemaVersion !== "normal-ui-acceptance-driver.v1") fail("NORMAL_UI_CONTEXT_VERSION");
  for (const key of ["runtimeRoot", "databasePath", "scopeRegistrationPath", "imageCheckpointPath", "auditPath", "normalSha256"]) {
    if (typeof context[key] !== "string" || !context[key]) fail("NORMAL_UI_CONTEXT_" + key.toUpperCase() + "_MISSING");
  }
  for (const key of ["databasePath", "scopeRegistrationPath", "imageCheckpointPath", "auditPath"]) context[key] = assertDChild(context.runtimeRoot, context[key], "NORMAL_UI_CONTEXT_" + key.toUpperCase());
  return context;
}
function provisionalRuntimeRoot(path) {
  if (typeof path !== "string" || basename(path) !== "ui-driver-context.json") fail("NORMAL_UI_CONTEXT_PATH_INVALID");
  const root = dirname(resolve(path));
  assertDChild(root, resolve(path), "NORMAL_UI_CONTEXT_PATH");
  return root;
}
const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const supervisorPath = resolve(scriptDirectory, "supervise-normal-ui-acceptance.ps1");
const stableUiaHelper = resolve(scriptDirectory, "select-export-directory-uia.ps1");

function fileSha256(path) {
  return "sha256:" + createHash("sha256").update(readFileSync(path)).digest("hex");
}

function selectedUiaHelper() {
  if (!uiaHelperOverride) {
    if (!existsSync(stableUiaHelper)) fail("NORMAL_UI_UIA_HELPER_REQUIRED");
    return stableUiaHelper;
  }
  const candidate = resolve(uiaHelperOverride);
  assertDChild(dirname(candidate), candidate, "NORMAL_UI_UIA_OVERRIDE");
  if (!existsSync(candidate) || !/^sha256:[a-f0-9]{64}$/.test(uiaHelperOverrideSha256)) fail("NORMAL_UI_UIA_OVERRIDE_INVALID");
  if (fileSha256(candidate) !== uiaHelperOverrideSha256) fail("NORMAL_UI_UIA_OVERRIDE_HASH_MISMATCH");
  return candidate;
}
function atomicJson(path, value) {
  mkdirSync(dirname(path), { recursive: true });
  const temporary = path + "." + randomBytes(8).toString("hex") + ".tmp";
  const fd = openSync(temporary, "w", 0o600);
  try { writeFileSync(fd, JSON.stringify(value)); fsyncSync(fd); } finally { closeSync(fd); }
  renameSync(temporary, path);
}
function registerScope(context, novel, chapter, body) {
  const helper = resolve(scriptDirectory, "register_ui_scope.py");
  const contentHash = "sha256:" + createHash("sha256").update(body, "utf8").digest("hex");
  const result = spawnSync("py", ["-3", helper, "--database", context.databasePath, "--novel", novel, "--chapter", chapter, "--content-hash", contentHash, "--wait-ms", "30000"], {
    encoding: "utf8", windowsHide: true,
  });
  if (result.error || result.status !== 0) fail("NORMAL_UI_SCOPE_HELPER_UNAVAILABLE");
  let parsed;
  try { parsed = JSON.parse(result.stdout); } catch { fail("NORMAL_UI_SCOPE_HELPER_INVALID"); }
  const scope = parsed?.scope;
  if (!parsed?.ok || !scope || ["project_id", "work_id", "chapter_id", "revision_id", "job_id", "source_run_id"].some((key) => typeof scope[key] !== "string" || !scope[key])) fail("NORMAL_UI_SCOPE_REJECTED:" + String(parsed?.code || "UNKNOWN"));
  atomicJson(context.scopeRegistrationPath, {
    schemaVersion: "normal-ui-acceptance-scope.v1",
    projectId: scope.project_id, novelWorkId: scope.work_id, novelChapterId: scope.chapter_id, sourceRevisionId: scope.revision_id,
    productionJobId: scope.job_id, sourceAnalysisRunId: scope.source_run_id,
  });
  return scope;
}
function verifyManualCandidate(context, beforePath) {
  const helper = resolve(scriptDirectory, "verify_manual_candidate.py");
  const result = spawnSync("py", ["-3", helper, "--database", context.databasePath, "--scope", context.scopeRegistrationPath].concat(beforePath ? ["--before", beforePath] : []), { encoding: "utf8", windowsHide: true });
  if (result.error || result.status !== 0) fail("NORMAL_UI_MANUAL_VERIFY_UNAVAILABLE");
  let parsed; try { parsed = JSON.parse(result.stdout); } catch { fail("NORMAL_UI_MANUAL_VERIFY_INVALID"); }
  if (!parsed?.ok || !parsed.snapshot) fail("NORMAL_UI_MANUAL_VERIFY_REJECTED:" + String(parsed?.code || "UNKNOWN"));
  return parsed.snapshot;
}
function plan(context) {
  return {
    schemaVersion: "normal-ui-driver-plan.v1",
    normalSha256: context.normalSha256,
    steps: [
      "Click 小说漫画, create/select isolated project and novel, then 新建章节.",
      "Type ordinary chapter text, select 漫画篇幅：一页五格（不规则）, and click 保存并全自动生产.",
      "RO-check the exact saved revision in SQLite, atomically register project/work/chapter/revision scope, and capture stage screenshots.",
      "Pause at imageCheckpointPath for root review; only the supervising proxy releases image after approval.",
      "After candidate_ready, scroll visible 导出本页 into the viewport, mouse-click it, then use only owner-PID UIA for the native folder dialog.",
      "Optionally make one harmless artifact candidate text edit; never preview/adopt or use AI.",
    ],
    prohibited: ["Tauri invoke", "store injection", "provider call", "retry", "adopt"],
  };
}
function assertCdpListenerOwnedByNormal(port, pid) {
  // port and pid have already been parsed as bounded integers. The command has
  // no user-provided PowerShell expression or path interpolation.
  const command = [
    "$listener=(Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort " + port + " -State Listen -ErrorAction Stop | Select-Object -First 1).OwningProcess",
    "if($null -eq $listener){exit 2}",
    "$rows=Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId",
    "$ids=[Collections.Generic.HashSet[int]]::new();$queue=[Collections.Generic.Queue[int]]::new();$queue.Enqueue(" + pid + ")",
    "while($queue.Count){$current=$queue.Dequeue();if(-not $ids.Add($current)){continue};foreach($row in $rows){if([int]$row.ParentProcessId -eq $current){$queue.Enqueue([int]$row.ProcessId)}}}",
    "if($ids.Contains([int]$listener)){exit 0};exit 3",
  ].join(";");
  const result = spawnSync("pwsh.exe", ["-NoProfile", "-Command", command], { windowsHide: true });
  if (result.error || result.status !== 0) fail("NORMAL_UI_CDP_OWNER_MISMATCH");
}
function assertLiveNormalIdentity(current) {
  const normalPath = assertDChild(current.runtimeRoot, current.normalExecutablePath, "NORMAL_UI_CONTEXT_NORMAL_EXE");
  const expectedStart = String(current.normalStartedAtUtc || "");
  const expectedSourceHash = String(current.normalSha256 || "").toUpperCase();
  const expectedCopiedHash = String(current.copiedNormalSha256 || "").toUpperCase();
  if (!expectedStart || !/^[0-9A-F]{64}$/.test(expectedSourceHash) || expectedCopiedHash !== expectedSourceHash) fail("NORMAL_UI_CONTEXT_NORMAL_IDENTITY_REQUIRED");
  const command = [
    "$process=Get-Process -Id $env:NORMAL_UI_PID -ErrorAction Stop",
    "$cim=Get-CimInstance Win32_Process -Filter ('ProcessId='+$env:NORMAL_UI_PID) -ErrorAction Stop",
    "$path=[IO.Path]::GetFullPath($process.Path);$expected=[IO.Path]::GetFullPath($env:NORMAL_UI_EXE)",
    "if(-not $path.Equals($expected,[StringComparison]::OrdinalIgnoreCase)){exit 2}",
    "if([string]::IsNullOrWhiteSpace([string]$cim.CommandLine) -or $cim.CommandLine.IndexOf($expected,[StringComparison]::OrdinalIgnoreCase)-lt 0){exit 3}",
    "$started=[DateTimeOffset]::Parse($env:NORMAL_UI_STARTED,[Globalization.CultureInfo]::InvariantCulture,[Globalization.DateTimeStyles]::RoundtripKind).UtcDateTime",
    "if([Math]::Abs(($process.StartTime.ToUniversalTime()-$started).TotalSeconds)-gt 5){exit 4}",
    "if($null -eq $cim.CreationDate -or [Math]::Abs((([datetime]$cim.CreationDate).ToUniversalTime()-$started).TotalSeconds)-gt 5){exit 6}",
    "if((Get-FileHash -LiteralPath $expected -Algorithm SHA256).Hash.ToUpperInvariant() -ne $env:NORMAL_UI_HASH){exit 5}",
  ].join(";");
  const result = spawnSync("pwsh.exe", ["-NoProfile", "-Command", command], {
    windowsHide: true,
    env: { ...process.env, NORMAL_UI_PID: String(normalPid), NORMAL_UI_EXE: normalPath, NORMAL_UI_STARTED: expectedStart, NORMAL_UI_HASH: expectedSourceHash },
  });
  if (result.error || result.status !== 0) fail("NORMAL_UI_LIVE_NORMAL_IDENTITY_MISMATCH");
}

async function connectVisiblePage(port) {
  const response = await fetch("http://127.0.0.1:" + port + "/json/list");
  const targets = await response.json();
  const target = targets.find((item) => item.type === "page" && item.webSocketDebuggerUrl && /^(tauri:|http:\/\/tauri\.localhost)/.test(item.url));
  if (!target) fail("NORMAL_UI_CDP_PAGE_MISSING");
  return target;
}
function withCdp(target, action) {
  return new Promise((resolveAction, rejectAction) => {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    let nextId = 1; const pending = new Map();
    socket.onopen = async () => {
      const send = (method, params = {}) => new Promise((resolveResult, rejectResult) => {
        const id = nextId++; pending.set(id, { resolveResult, rejectResult }); socket.send(JSON.stringify({ id, method, params }));
      });
      try { resolveAction(await action(send)); } catch (error) { rejectAction(error); } finally { socket.close(); }
    };
    socket.onmessage = ({ data }) => { const message = JSON.parse(data); const item = pending.get(message.id); if (!item) return; pending.delete(message.id); message.error ? item.rejectResult(new Error(message.error.message)) : item.resolveResult(message.result); };
    socket.onerror = () => rejectAction(new Error("NORMAL_UI_CDP_SOCKET_ERROR"));
  });
}
async function documentNodes(send, selector) {
  const { root } = await send("DOM.getDocument", { depth: -1 });
  return (await send("DOM.querySelectorAll", { nodeId: root.nodeId, selector })).nodeIds;
}
async function textFor(send, nodeId) { return (await send("DOM.getOuterHTML", { nodeId })).outerHTML.replace(/<[^>]+>/g, "").trim(); }
async function visiblePoint(send, nodeId) {
  await send("DOM.scrollIntoViewIfNeeded", { nodeId });
  const { model } = await send("DOM.getBoxModel", { nodeId }); const quad = model.content;
  const point = { x: (quad[0] + quad[2] + quad[4] + quad[6]) / 4, y: (quad[1] + quad[3] + quad[5] + quad[7]) / 4 };
  const viewport = (await send("Runtime.evaluate", { expression: "({w:innerWidth,h:innerHeight})", returnByValue: true })).result.value;
  if (!(point.x >= 0 && point.y >= 0 && point.x <= viewport.w && point.y <= viewport.h)) fail("NORMAL_UI_CONTROL_NOT_VISIBLE");
  return point;
}
const delay = (milliseconds) => new Promise((done) => setTimeout(done, milliseconds));
async function uniqueVisibleNode(send, selector, matches, timeoutMs, code) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const candidates = [];
    for (const nodeId of await documentNodes(send, selector)) {
      if (!(await matches(nodeId))) continue;
      try { candidates.push({ nodeId, point: await visiblePoint(send, nodeId) }); } catch { /* Hidden/unlaid-out matches are not clickable candidates. */ }
    }
    if (candidates.length === 1) return candidates[0];
    if (candidates.length > 1) fail(code + "_AMBIGUOUS");
    await delay(200);
  }
  fail(code + "_MISSING");
}
async function clickText(send, expected) {
  const { point } = await uniqueVisibleNode(send, "button,[role=tab]", async (nodeId) => (await textFor(send, nodeId)) === expected, 10000, "NORMAL_UI_CONTROL:" + expected);
  await send("Input.dispatchMouseEvent", { type: "mousePressed", ...point, button: "left", clickCount: 1 });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", ...point, button: "left", clickCount: 1 });
  await delay(100);
}
async function typeIntoPlaceholder(send, placeholder, value) {
  const { point } = await uniqueVisibleNode(send, "input,textarea", async (nodeId) => (await send("DOM.getOuterHTML", { nodeId })).outerHTML.includes('placeholder="' + placeholder + '"'), 10000, "NORMAL_UI_INPUT:" + placeholder);
  await send("Input.dispatchMouseEvent", { type: "mousePressed", ...point, button: "left", clickCount: 1 });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", ...point, button: "left", clickCount: 1 });
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: "a", code: "KeyA", windowsVirtualKeyCode: 65, modifiers: 2 });
  await send("Input.dispatchKeyEvent", { type: "keyUp", key: "a", code: "KeyA", windowsVirtualKeyCode: 65, modifiers: 2 });
  await send("Input.insertText", { text: value });
}
async function typeIntoAria(send, label, value) {
  const { point } = await uniqueVisibleNode(send, "input,textarea", async (nodeId) => (await send("DOM.getOuterHTML", { nodeId })).outerHTML.includes('aria-label="' + label + '"'), 10000, "NORMAL_UI_ARIA_INPUT:" + label);
  await send("Input.dispatchMouseEvent", { type: "mousePressed", ...point, button: "left", clickCount: 1 });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", ...point, button: "left", clickCount: 1 });
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: "a", code: "KeyA", windowsVirtualKeyCode: 65, modifiers: 2 });
  await send("Input.dispatchKeyEvent", { type: "keyUp", key: "a", code: "KeyA", windowsVirtualKeyCode: 65, modifiers: 2 });
  await send("Input.insertText", { text: value });
}
async function selectVisibleValue(send, ariaLabel, value) {
  const selected = await uniqueVisibleNode(send, "select", async (nodeId) => {
    const { outerHTML } = await send("DOM.getOuterHTML", { nodeId });
    return outerHTML.includes('aria-label="' + ariaLabel + '"') && outerHTML.includes('value="' + value + '"');
  }, 10000, "NORMAL_UI_SELECT:" + ariaLabel);
  const { outerHTML } = await send("DOM.getOuterHTML", { nodeId: selected.nodeId });
  await send("Input.dispatchMouseEvent", { type: "mousePressed", ...selected.point, button: "left", clickCount: 1 });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", ...selected.point, button: "left", clickCount: 1 });
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: "Home", code: "Home", windowsVirtualKeyCode: 36 });
  const index = [...outerHTML.matchAll(/<option\b[^>]*value="([^"]+)"/g)].map((match) => match[1]).indexOf(value);
  for (let position = 0; position < index; position++) await send("Input.dispatchKeyEvent", { type: "keyDown", key: "ArrowDown", code: "ArrowDown", windowsVirtualKeyCode: 40 });
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
  const deadline = Date.now() + 3000;
  while (Date.now() < deadline) { if (await valueForNode(send, selected.nodeId) === value) return; await delay(100); }
  fail("NORMAL_UI_SELECT_VALUE_NOT_APPLIED:" + ariaLabel);
}
async function waitForFile(path, timeoutMs, code) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) { if (existsSync(path)) return; await new Promise((done) => setTimeout(done, 250)); }
  fail(code);
}
async function waitForCandidateImage(send) {
  const deadline = Date.now() + 300000;
  while (Date.now() < deadline) {
    const nodes = await documentNodes(send, '[aria-label="本章漫画页"] img');
    if (nodes.length === 1) {
      const state = (await send("Runtime.evaluate", { expression: "(() => { const x=document.querySelectorAll('[aria-label=\"本章漫画页\"] img'); if(x.length!==1)return null; const i=x[0];return {alt:i.alt,complete:i.complete,w:i.naturalWidth,h:i.naturalHeight}; })()", returnByValue: true })).result.value;
      if (state?.alt === "第 1 页漫画" && state.complete && state.w === 1024 && state.h === 1536) return state;
    }
    await new Promise((done) => setTimeout(done, 500));
  }
  fail("NORMAL_UI_CANDIDATE_IMAGE_NOT_READY");
}
async function captureSafeFailure(send, context, code) {
  try {
    const capture = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
    const screenshot = assertDChild(context.runtimeRoot, resolve(context.runtimeRoot, "screens", "driver-failure.png"), "NORMAL_UI_FAILURE_SCREENSHOT");
    mkdirSync(dirname(screenshot), { recursive: true });
    if (!existsSync(screenshot)) writeFileSync(screenshot, Buffer.from(capture.data, "base64"), { flag: "wx" });
    const page = (await send("Runtime.evaluate", { expression: "({title:document.title,url:location.href})", returnByValue: true })).result.value;
    atomicJson(assertDChild(context.runtimeRoot, resolve(context.runtimeRoot, "driver-failure.json"), "NORMAL_UI_FAILURE_METADATA"), { code, screenshot, page });
  } catch { /* Preserve the original failure; diagnostics are best-effort only. */ }
}
async function valueForNode(send, nodeId) {
  const { object } = await send("DOM.resolveNode", { nodeId });
  return (await send("Runtime.callFunctionOn", { objectId: object.objectId, functionDeclaration: "function(){return this.value}", returnByValue: true })).result.value;
}
async function waitForText(send, expected, timeoutMs, code) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    for (const nodeId of await documentNodes(send, "body *")) if ((await textFor(send, nodeId)) === expected) return;
    await new Promise((done) => setTimeout(done, 250));
  }
  fail(code);
}
async function manualWorldCandidateEdit(send) {
  await clickText(send, "文字产物");
  const artifact = await uniqueVisibleNode(send, "button", async (nodeId) => {
    const text = await textFor(send, nodeId);
    return text === "世界观本章 · 已采用" || text === "世界观本章·已采用";
  }, 10000, "NORMAL_UI_WORLD_ARTIFACT");
  await send("Input.dispatchMouseEvent", { type: "mousePressed", ...artifact.point, button: "left", clickCount: 1 });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", ...artifact.point, button: "left", clickCount: 1 });
  await clickText(send, "编辑");
  const field = await uniqueVisibleNode(send, '[data-artifact-dialog] main textarea:not([disabled]),[data-artifact-dialog] main input:not([disabled])', async (nodeId) => {
    const { outerHTML } = await send("DOM.getOuterHTML", { nodeId });
    if (/json/i.test(outerHTML)) return false;
    const value = await valueForNode(send, nodeId);
    return typeof value === "string" && Boolean(value.trim());
  }, 10000, "NORMAL_UI_WORLD_NATURAL_LANGUAGE_FIELD");
  await send("Input.dispatchMouseEvent", { type: "mousePressed", ...field.point, button: "left", clickCount: 1 });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", ...field.point, button: "left", clickCount: 1 });
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: "End", code: "End", windowsVirtualKeyCode: 35 });
  await send("Input.insertText", { text: "（验收手动微调）" });
  await clickText(send, "保存候选");
  await waitForText(send, "待审阅", 10000, "NORMAL_UI_WORLD_CANDIDATE_STATUS_TIMEOUT");
}
function selectExportDirectory(context, uiaHelper) {
  const directory = assertDChild(context.runtimeRoot, resolve(context.runtimeRoot, "export"), "NORMAL_UI_EXPORT_DIRECTORY");
  mkdirSync(directory, { recursive: true });
  const result = spawnSync("pwsh.exe", ["-NoProfile", "-File", uiaHelper, "-Select", "-OwnerProcessId", String(normalPid), "-ExpectedOwnerExe", context.normalExecutablePath, "-AllowedRoot", context.runtimeRoot, "-Directory", directory, "-TimeoutSeconds", "20"], { encoding: "utf8", windowsHide: true });
  if (result.error || result.status !== 0) {
    const detail = `${result.stdout || ""}\n${result.stderr || ""}`.match(/UIA_EXPORT_[A-Z_]+/)?.[0] || "FAILED";
    fail(`NORMAL_UI_UIA_EXPORT_DIRECTORY_${detail}`);
  }
  let parsed; try { parsed = JSON.parse(result.stdout); } catch { fail("NORMAL_UI_UIA_EXPORT_DIRECTORY_INVALID"); }
  if (parsed?.mode !== "selected" || parsed?.directory !== directory) fail("NORMAL_UI_UIA_EXPORT_DIRECTORY_REJECTED");
  return directory;
}
function verifyUiExport(context, exportRoot) {
  const helper = resolve(scriptDirectory, "verify_ui_export.py");
  const result = spawnSync("py", ["-3", helper, "--database", context.databasePath, "--scope", context.scopeRegistrationPath, "--export-root", exportRoot], { encoding: "utf8", windowsHide: true });
  if (result.error || result.status !== 0) fail("NORMAL_UI_EXPORT_VERIFY_UNAVAILABLE");
  let parsed; try { parsed = JSON.parse(result.stdout); } catch { fail("NORMAL_UI_EXPORT_VERIFY_INVALID"); }
  if (!parsed?.ok || typeof parsed.sha256 !== "string") fail("NORMAL_UI_EXPORT_VERIFY_REJECTED:" + String(parsed?.code || "UNKNOWN"));
  return parsed;
}
function stopAndVerify(context, requireCompleted) {
  const result = spawnSync("pwsh.exe", ["-NoProfile", "-File", stopSupervisor, "-Stop", "-RuntimeRoot", context.runtimeRoot], { encoding: "utf8", windowsHide: true });
  if (result.error || result.status !== 0) fail("NORMAL_UI_STOP_FAILED");
  const statusPath = assertDChild(context.runtimeRoot, resolve(context.runtimeRoot, "supervisor-status.json"), "NORMAL_UI_STOP_STATUS");
  const status = JSON.parse(readFileSync(statusPath, "utf8"));
  if (status.status !== "stopped" || status.cdpPortReleased !== true) fail("NORMAL_UI_STOP_NOT_CONFIRMED");
  if (!requireCompleted) return;
  const forwards = status.forwards;
  const expectedStops = new Set(["NORMAL_UI_STOP_NORMAL_VERIFIED", "NORMAL_UI_STOP_NORMAL_ALREADY_STOPPED", "NORMAL_UI_STOP_PROXY_VERIFIED", "NORMAL_UI_STOP_PROXY_ALREADY_STOPPED"]);
  if (!forwards || forwards.source !== 1 || forwards.adaptation !== 1 || forwards.image !== 1 || status.configurationCleanup !== "removed-exact-loopback-config" || !Array.isArray(status.stopVerification) || !status.stopVerification.some((code) => expectedStops.has(code) && code.includes("NORMAL")) || !status.stopVerification.some((code) => expectedStops.has(code) && code.includes("PROXY"))) fail("NORMAL_UI_COMPLETION_STOP_STATUS_INVALID");
  const readyPath = assertDChild(context.runtimeRoot, resolve(context.runtimeRoot, "proxy-ready.json"), "NORMAL_UI_PROXY_READY");
  const ready = JSON.parse(readFileSync(readyPath, "utf8"));
  const command = "if(Get-Process -Id $env:NORMAL_UI_PID -ErrorAction SilentlyContinue){exit 2};if(Get-Process -Id $env:NORMAL_UI_PROXY_PID -ErrorAction SilentlyContinue){exit 3};if(Get-NetTCPConnection -LocalPort $env:NORMAL_UI_CDP_PORT -State Listen -ErrorAction SilentlyContinue){exit 4};if(Get-NetTCPConnection -LocalPort $env:NORMAL_UI_PROXY_PORT -State Listen -ErrorAction SilentlyContinue){exit 5}";
  const clean = spawnSync("pwsh.exe", ["-NoProfile", "-Command", command], { windowsHide: true, env: { ...process.env, NORMAL_UI_PID: String(status.normalPid), NORMAL_UI_PROXY_PID: String(status.proxyPid), NORMAL_UI_CDP_PORT: String(status.cdpPort), NORMAL_UI_PROXY_PORT: String(ready.port) } });
  if (clean.error || clean.status !== 0) fail("NORMAL_UI_COMPLETION_PROCESS_OR_PORT_RETAINED");
}
async function runFakeCdpDomTest() {
  const calls = [];
  const attributes = new Map([
    [2, '<select aria-label="漫画篇幅"><option value="automatic">整章自动</option><option value="hero_middle_5">一页五格（不规则）</option></select>'],
    [3, '<textarea aria-label="章节正文"></textarea>'],
  ]);
  const send = async (method, params = {}) => {
    calls.push({ method, params });
    if (method === "DOM.getDocument") return { root: { nodeId: 1 } };
    if (method === "DOM.querySelectorAll") return { nodeIds: [2, 3] };
    if (method === "DOM.getOuterHTML") return { outerHTML: attributes.get(params.nodeId) || "" };
    if (method === "DOM.getBoxModel") return { model: { content: [10, 10, 110, 10, 110, 30, 10, 30] } };
    if (method === "DOM.resolveNode") return { object: { objectId: "fake-node-" + params.nodeId } };
    if (method === "Runtime.callFunctionOn") return { result: { value: "hero_middle_5" } };
    if (method === "Runtime.evaluate") return { result: { value: { w: 800, h: 600 } } };
    return {};
  };
  await selectVisibleValue(send, "漫画篇幅", "hero_middle_5");
  await typeIntoAria(send, "章节正文", "成年人雨巷正文");
  const arrowDowns = calls.filter((call) => call.method === "Input.dispatchKeyEvent" && call.params.key === "ArrowDown").length;
  const typed = calls.some((call) => call.method === "Input.insertText" && call.params.text === "成年人雨巷正文");
  const outOfViewport = calls.some((call) => call.method === "Input.dispatchMouseEvent" && (call.params.x < 0 || call.params.y < 0 || call.params.x > 800 || call.params.y > 600));
  const malformed = spawnSync(process.execPath, [fileURLToPath(import.meta.url), "--self-test", "--run"], { windowsHide: true });
  const corruptRoot = resolve(scriptDirectory, "..", "..", ".test-tmp", "normal-ui-driver-implementation-20260905", "corrupt-context-" + randomBytes(4).toString("hex"));
  mkdirSync(corruptRoot, { recursive: true });
  const corruptContext = resolve(corruptRoot, "ui-driver-context.json"); writeFileSync(corruptContext, "{", { flag: "wx" });
  const cleanupRoot = provisionalRuntimeRoot(corruptContext);
  let corruptRejected = false; try { readContext(corruptContext); } catch { corruptRejected = true; }
  if (arrowDowns !== 1 || !typed || outOfViewport || malformed.status === 0 || !corruptRejected || cleanupRoot !== corruptRoot) fail("NORMAL_UI_DRIVER_FAKE_CDP_ASSERTION");
  process.stdout.write(JSON.stringify({ ok: true, test: "fake-cdp-select-input-and-corrupt-context-cleanup-decision", arrowDowns, typed }) + "\n");
}
async function main() {
  // Validate the immutable default helper (or an explicitly pinned D-only
  // override) before any UI interaction.  A bad override must not be allowed
  // to survive until the post-generation export step.
  const uiaHelper = selectedUiaHelper();
  if (!run) {
    context = readContext(contextPath);
    process.stdout.write(JSON.stringify({ mode: "dry-run", plan: plan(context) }, null, 2) + "\n");
    return;
  }
  let provisionalRoot;
  try {
  provisionalRoot = provisionalRuntimeRoot(contextPath);
  if (!existsSync(supervisorPath)) fail("NORMAL_UI_STOP_SUPERVISOR_REQUIRED");
  stopSupervisor = supervisorPath;
  context = readContext(contextPath);
  if (context.runtimeRoot !== provisionalRoot) fail("NORMAL_UI_CONTEXT_RUNTIME_MISMATCH");
  cdpPort = Number(context.cdpPort);
  normalPid = Number(context.normalPid);
  if (!Number.isInteger(cdpPort) || cdpPort < 1 || cdpPort > 65535 || !Number.isInteger(normalPid) || normalPid < 1) fail("NORMAL_UI_CONTEXT_VISIBLE_PID_AND_CDP_REQUIRED");
  if (typeof context.normalExecutablePath !== "string" || !existsSync(context.normalExecutablePath)) fail("NORMAL_UI_CONTEXT_NORMAL_EXE_REQUIRED");
  if (!stopSupervisor || typeof context.stopSupervisorPath !== "string" || resolve(context.stopSupervisorPath) !== supervisorPath) fail("NORMAL_UI_STOP_SUPERVISOR_REQUIRED");
  assertLiveNormalIdentity(context);
  assertCdpListenerOwnedByNormal(cdpPort, normalPid);
  const target = await connectVisiblePage(cdpPort);
  await withCdp(target, async (send) => {
    try {
      await send("DOM.enable"); await send("Page.enable");
      const marker = randomBytes(4).toString("hex");
      const novel = "验收小说-" + marker;
      const chapter = "验收章节-" + marker;
      const body = "雨巷的路灯映着积水。林岚握着红伞说：别再躲了。周野提起旧相机，轻声回答：我一直在等你。啪嗒，雨滴落在信封上；哐当，铁门被风推开。两人都是成年人，周野把银色钥匙交给林岚，林岚喊道：现在一起走。";
      await clickText(send, "小说漫画");
      await clickText(send, "新建小说");
      await typeIntoPlaceholder(send, "小说名称", novel);
      await clickText(send, "创建小说");
      await clickText(send, "新建章节");
      await typeIntoPlaceholder(send, "章节标题", chapter);
      await typeIntoAria(send, "章节正文", body);
      await selectVisibleValue(send, "漫画篇幅", "hero_middle_5");
      await clickText(send, "保存并全自动生产");
      const scope = registerScope(context, novel, chapter, body);
      const capture = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
      const evidencePath = assertDChild(context.runtimeRoot, resolve(context.runtimeRoot, "screens", "after-save-source-running.png"), "NORMAL_UI_SCREENSHOT");
      mkdirSync(dirname(evidencePath), { recursive: true }); writeFileSync(evidencePath, Buffer.from(capture.data, "base64"), { flag: "wx" });
      process.stdout.write(JSON.stringify({ event: "scope-registered", scope, screenshot: evidencePath }) + "\n");
      await waitForFile(context.imageCheckpointPath, 300000, "NORMAL_UI_IMAGE_CHECKPOINT_TIMEOUT");
      const checkpointCapture = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
      const checkpointPath = assertDChild(context.runtimeRoot, resolve(context.runtimeRoot, "screens", "image-checkpoint.png"), "NORMAL_UI_CHECKPOINT_SCREENSHOT");
      mkdirSync(dirname(checkpointPath), { recursive: true }); writeFileSync(checkpointPath, Buffer.from(checkpointCapture.data, "base64"), { flag: "wx" });
      process.stdout.write(JSON.stringify({ event: "image-checkpoint-observed", scope, screenshot: checkpointPath }) + "\n");
      const image = await waitForCandidateImage(send);
      const candidateCapture = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
      const candidatePath = assertDChild(context.runtimeRoot, resolve(context.runtimeRoot, "screens", "candidate-ready.png"), "NORMAL_UI_CANDIDATE_SCREENSHOT");
      mkdirSync(dirname(candidatePath), { recursive: true }); writeFileSync(candidatePath, Buffer.from(candidateCapture.data, "base64"), { flag: "wx" });
      await clickText(send, "导出本页");
      await waitForText(send, "导出中…", 5000, "NORMAL_UI_EXPORT_PENDING_NOT_VISIBLE");
      const exportDirectory = selectExportDirectory(context, uiaHelper);
      const manualBeforePath = assertDChild(context.runtimeRoot, resolve(context.runtimeRoot, "manual-world-before.json"), "NORMAL_UI_MANUAL_BEFORE");
      atomicJson(manualBeforePath, verifyManualCandidate(context));
      await waitForText(send, "打开导出文件夹", 30000, "NORMAL_UI_EXPORT_SUCCESS_TIMEOUT");
      await waitForText(send, "导出本页", 30000, "NORMAL_UI_EXPORT_PENDING_NOT_CLEARED");
      const exported = verifyUiExport(context, exportDirectory);
      process.stdout.write(JSON.stringify({ event: "candidate-ui-exported", image, screenshot: candidatePath, exportDirectory, exported }) + "\n");
      await manualWorldCandidateEdit(send);
      verifyManualCandidate(context, manualBeforePath);
      process.stdout.write(JSON.stringify({ event: "manual-world-candidate-saved", scope }) + "\n");
      completed = true;
    } catch (error) {
      await captureSafeFailure(send, context, error instanceof Error ? error.message : "NORMAL_UI_UNKNOWN_FAILURE");
      throw error;
    }
  });
  process.stdout.write(JSON.stringify({ event: "normal-ui-driver-complete" }) + "\n");
  } catch (error) {
    process.stdout.write(JSON.stringify({ event: "normal-ui-driver-failed", code: error instanceof Error ? error.message : "NORMAL_UI_DRIVER_FAILED" }) + "\n");
    throw error;
  } finally {
    if (provisionalRoot && stopSupervisor) stopAndVerify(context ?? { runtimeRoot: provisionalRoot }, completed);
  }
}

if (selfTest) await runFakeCdpDomTest();
else await main();
