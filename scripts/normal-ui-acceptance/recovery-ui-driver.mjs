#!/usr/bin/env node
// Provider-free recovery export acceptance. It only drives visible UI and
// verifies the Rust-owned default export receipt; it never opens a native
// folder chooser, invokes Tauri commands directly, or changes a candidate.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const sleep = (milliseconds) => new Promise((resolveSleep) => setTimeout(resolveSleep, milliseconds));
const fail = (code) => { throw new Error(code); };
const args = process.argv.slice(2);
if (!((args.length === 2 || args.length === 3) && args[0] === "--status" && (args.length === 2 || args[2] === "--export-only"))) fail("RECOVERY_EXPORT_USAGE");
const statusPath = resolve(args[1]);
let status;

function safeChild(root, value, code) {
  const rootPath = resolve(root);
  const full = resolve(value);
  const rel = relative(rootPath, full);
  if (!/^D:[\\/]/i.test(full) || rel === "" || rel === ".." || rel.startsWith("..\\") || rel.startsWith("../")) fail(code);
  return full;
}

function readStatus() {
  const parsed = JSON.parse(readFileSync(statusPath, "utf8"));
  if (parsed?.schemaVersion !== "normal-ui-recovery-live.v1" || parsed.status !== "running") fail("RECOVERY_STATUS_INVALID");
  for (const key of ["runtimeRoot", "databasePath", "scopePath", "stopScriptPath", "normalExecutablePath"]) if (typeof parsed[key] !== "string" || !parsed[key]) fail("RECOVERY_STATUS_INVALID");
  if (!Number.isInteger(parsed.cdpPort) || parsed.cdpPort < 1 || parsed.cdpPort > 65535) fail("RECOVERY_STATUS_INVALID");
  return parsed;
}

function stop() {
  const result = spawnSync("pwsh.exe", ["-NoProfile", "-File", status.stopScriptPath, "-Stop", "-RuntimeRoot", status.runtimeRoot], { encoding: "utf8", windowsHide: true });
  if (result.status !== 0) fail("RECOVERY_STOP_FAILED");
}

async function target(port) {
  for (let until = Date.now() + 30_000; Date.now() < until; await sleep(200)) {
    try {
      const entries = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      const page = entries.find((entry) => entry.type === "page" && entry.webSocketDebuggerUrl && /^(tauri:|http:\/\/tauri\.localhost)/.test(entry.url));
      if (page) return page;
    } catch { /* wait for this isolated application's CDP endpoint */ }
  }
  fail("RECOVERY_CDP_TARGET_TIMEOUT");
}

function cdp(page, fn) {
  return new Promise((resolveRun, rejectRun) => {
    const socket = new WebSocket(page.webSocketDebuggerUrl);
    const pending = new Map();
    let nextId = 1;
    socket.onopen = async () => {
      const send = (method, params = {}) => new Promise((resolveRequest, rejectRequest) => {
        const id = nextId++;
        pending.set(id, { resolveRequest, rejectRequest });
        socket.send(JSON.stringify({ id, method, params }));
      });
      try { resolveRun(await fn(send)); } catch (error) { rejectRun(error); } finally { socket.close(); }
    };
    socket.onmessage = ({ data }) => {
      const message = JSON.parse(data);
      const item = pending.get(message.id);
      if (!item) return;
      pending.delete(message.id);
      if (message.error) item.rejectRequest(new Error(message.error.message));
      else item.resolveRequest(message.result);
    };
    socket.onerror = () => rejectRun(new Error("RECOVERY_CDP_SOCKET"));
  });
}

async function nodes(send, selector) {
  const { root } = await send("DOM.getDocument", { depth: -1 });
  return (await send("DOM.querySelectorAll", { nodeId: root.nodeId, selector })).nodeIds;
}

async function text(send, nodeId) {
  return (await send("DOM.getOuterHTML", { nodeId })).outerHTML.replace(/<[^>]+>/g, "").trim();
}

async function visiblePoint(send, nodeId) {
  await send("DOM.scrollIntoViewIfNeeded", { nodeId });
  const box = (await send("DOM.getBoxModel", { nodeId })).model.content;
  const point = { x: (box[0] + box[2] + box[4] + box[6]) / 4, y: (box[1] + box[3] + box[5] + box[7]) / 4 };
  const viewport = (await send("Runtime.evaluate", { expression: "({ width: innerWidth, height: innerHeight })", returnByValue: true })).result.value;
  if (!viewport || point.x < 0 || point.y < 0 || point.x > viewport.width || point.y > viewport.height) fail("RECOVERY_CONTROL_NOT_VISIBLE");
  return { ...point, viewport };
}

async function uniqueVisible(send, selector, predicate, code) {
  for (let until = Date.now() + 10_000; Date.now() < until; await sleep(150)) {
    const matches = [];
    for (const nodeId of await nodes(send, selector)) {
      try {
        if (await predicate(nodeId)) {
          await visiblePoint(send, nodeId);
          matches.push(nodeId);
        }
      } catch { /* hidden or stale candidates are not interactable */ }
    }
    if (matches.length === 1) return matches[0];
    if (matches.length > 1) fail(`${code}_AMBIGUOUS`);
  }
  fail(`${code}_MISSING`);
}

async function mouseClick(send, nodeId) {
  const point = await visiblePoint(send, nodeId);
  await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: point.x, y: point.y });
  await send("Input.dispatchMouseEvent", { type: "mousePressed", x: point.x, y: point.y, button: "left", clickCount: 1 });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", x: point.x, y: point.y, button: "left", clickCount: 1 });
  return point;
}

async function clickExact(send, value, code) {
  const control = await uniqueVisible(send, "button,[role=tab]", async (nodeId) => (await text(send, nodeId)) === value, code);
  return mouseClick(send, control);
}

async function selectValue(send, value) {
  const select = await uniqueVisible(send, "select", async (nodeId) => (await send("DOM.getOuterHTML", { nodeId })).outerHTML.includes(`value="${value}"`), `RECOVERY_SELECT:${value}`);
  const html = (await send("DOM.getOuterHTML", { nodeId: select })).outerHTML;
  const ordinal = [...html.matchAll(/<option\b[^>]*value="([^"]+)"/g)].map((match) => match[1]).indexOf(value);
  if (ordinal < 0) fail(`RECOVERY_SELECT:${value}_VALUE_MISSING`);
  await mouseClick(send, select);
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: "Home", windowsVirtualKeyCode: 36 });
  for (let index = 0; index < ordinal; index += 1) await send("Input.dispatchKeyEvent", { type: "keyDown", key: "ArrowDown", windowsVirtualKeyCode: 40 });
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: "Enter", windowsVirtualKeyCode: 13 });
}

async function waitForButton(send, value, timeout, code) {
  for (let until = Date.now() + timeout; Date.now() < until; await sleep(150)) {
    const matches = [];
    for (const nodeId of await nodes(send, "button")) {
      try { if ((await text(send, nodeId)) === value) matches.push(nodeId); } catch { /* stale */ }
    }
    if (matches.length === 1) return matches[0];
    if (matches.length > 1) fail(`${code}_AMBIGUOUS`);
  }
  fail(code);
}

async function waitForImage(send) {
  for (let until = Date.now() + 30_000; Date.now() < until; await sleep(250)) {
    const result = (await send("Runtime.evaluate", {
      expression: "(()=>{const images=document.querySelectorAll('[aria-label=\\\"本章漫画页\\\"] img');if(images.length!==1)return null;const image=images[0];return {alt:image.alt,complete:image.complete,width:image.naturalWidth,height:image.naturalHeight}})()",
      returnByValue: true,
    })).result.value;
    if (result?.alt === "第 1 页漫画" && result.complete && result.width === 1024 && result.height === 1536) return result;
  }
  fail("RECOVERY_IMAGE_NOT_READY");
}

function dataRoot() {
  return safeChild(status.runtimeRoot, resolve(status.runtimeRoot, "data"), "RECOVERY_DATA_ROOT");
}

function verifyDefaultExport(beforePath = null) {
  const helper = resolve(here, "verify_ui_export.py");
  const command = ["-3", helper, "--database", status.databasePath, "--scope", status.scopePath, "--data-root", dataRoot()];
  if (beforePath) command.push("--before-candidate", beforePath);
  const result = spawnSync("py", command, { encoding: "utf8", windowsHide: true });
  let parsed;
  try { parsed = JSON.parse(result.stdout); } catch { /* safe code below */ }
  if (result.status !== 0 || !parsed?.ok) fail(`RECOVERY_EXPORT_VERIFY:${String(parsed?.code || "UNAVAILABLE")}`);
  return parsed;
}

function snapshotCandidate(path) {
  const output = safeChild(status.runtimeRoot, path, "RECOVERY_EXPORT_BEFORE");
  const result = spawnSync("py", ["-3", resolve(here, "verify_ui_export.py"), "--database", status.databasePath, "--scope", status.scopePath, "--data-root", dataRoot(), "--snapshot-out", output], { encoding: "utf8", windowsHide: true });
  let parsed;
  try { parsed = JSON.parse(result.stdout); } catch { /* safe code below */ }
  if (result.status !== 0 || !parsed?.ok || !existsSync(output)) fail(`RECOVERY_EXPORT_SNAPSHOT:${String(parsed?.code || "UNAVAILABLE")}`);
  return output;
}

async function assertSecondaryExportAction(send) {
  await uniqueVisible(send, "button", async (nodeId) => (await text(send, nodeId)) === "选择文件夹导出…", "RECOVERY_SECONDARY_EXPORT");
}

async function exportDefaultPage(send) {
  const primary = await uniqueVisible(send, "button", async (nodeId) => {
    if ((await text(send, nodeId)) !== "导出本页") return false;
    return !/\sdisabled(?:=|[\s>])/i.test((await send("DOM.getOuterHTML", { nodeId })).outerHTML);
  }, "RECOVERY_DEFAULT_EXPORT");
  const click = await mouseClick(send, primary);
  await waitForButton(send, "导出中…", 10_000, "RECOVERY_EXPORT_PENDING_NOT_OBSERVED");
  await waitForButton(send, "打开导出文件夹", 30_000, "RECOVERY_EXPORT_RECEIPT_NOT_VISIBLE");
  await waitForButton(send, "导出本页", 5_000, "RECOVERY_EXPORT_PENDING_NOT_CLEARED");
  return click;
}

function writeEvidence(relativePath, value) {
  const path = safeChild(status.runtimeRoot, resolve(status.runtimeRoot, relativePath), "RECOVERY_EVIDENCE_PATH");
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(value), { flag: "wx" });
  return path;
}

try {
  status = readStatus();
  const scope = JSON.parse(readFileSync(status.scopePath, "utf8"));
  const beforePath = safeChild(status.runtimeRoot, resolve(status.runtimeRoot, "export-before-candidate.json"), "RECOVERY_EXPORT_BEFORE");
  snapshotCandidate(beforePath);
  const page = await target(status.cdpPort);
  await cdp(page, async (send) => {
    await send("DOM.enable");
    await send("Page.enable");
    await clickExact(send, "小说漫画", "RECOVERY_TAB_NOVEL_COMIC");
    await selectValue(send, scope.projectId);
    await selectValue(send, scope.novelWorkId);
    await selectValue(send, scope.novelChapterId);
    await clickExact(send, "漫画页", "RECOVERY_TAB_RESULTS");
    const image = await waitForImage(send);
    await assertSecondaryExportAction(send);
    const initial = await send("Page.captureScreenshot", { format: "png" });
    const screenshot = safeChild(status.runtimeRoot, resolve(status.runtimeRoot, "screens", "recovery-default-export-before.png"), "RECOVERY_SCREENSHOT");
    mkdirSync(dirname(screenshot), { recursive: true });
    writeFileSync(screenshot, Buffer.from(initial.data, "base64"), { flag: "wx" });
    const click = await exportDefaultPage(send);
    const finalShot = await send("Page.captureScreenshot", { format: "png" });
    const completedScreenshot = safeChild(status.runtimeRoot, resolve(status.runtimeRoot, "screens", "recovery-default-export-complete.png"), "RECOVERY_SCREENSHOT");
    writeFileSync(completedScreenshot, Buffer.from(finalShot.data, "base64"), { flag: "wx" });
    const exported = verifyDefaultExport(beforePath);
    writeEvidence("recovery-default-export-ui.json", { click, image, screenshot, completedScreenshot, exportId: exported.exportId, directory: exported.directory, sha256: exported.sha256 });
    process.stdout.write(JSON.stringify({ event: "recovery-default-export-complete", image, screenshot, completedScreenshot, exported }) + "\n");
  });
} catch (error) {
  const code = String(error?.message || error);
  if (status) {
    try { writeEvidence("recovery-default-export-failure.json", { code }); } catch { /* preserve original failure */ }
  }
  process.stdout.write(JSON.stringify({ event: "recovery-default-export-failed", code }) + "\n");
  throw error;
} finally {
  if (status) stop();
}
