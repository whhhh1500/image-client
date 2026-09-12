#!/usr/bin/env node
// 验证状态栏外链在打包版里真的能打开：用 CDP 连上正在运行的应用，点击按钮，
// 再检查应用日志里有没有 `github.repo_open_failed`（有 = 被 capability 拦截）。
//
//   node scripts/verify-external-links.mjs [--exe <便携版路径>]
//
// 注意：验证成功时系统浏览器会真的打开对应网页。

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const argv = process.argv.slice(2);
const arg = (name, fallback) => {
  const inline = argv.find((value) => value.startsWith(`--${name}=`));
  if (inline) return inline.slice(name.length + 3);
  const index = argv.indexOf(`--${name}`);
  return index >= 0 ? argv[index + 1] : fallback;
};

/** Newest portable build under release-exe/, so the default never goes stale. */
function newestPortable() {
  const base = resolve(process.cwd(), "release-exe");
  if (!existsSync(base)) throw new Error("没有 release-exe 目录，请用 --exe 指定便携版路径");
  const found = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (/^Image-Client_.*_x64_portable.*\.exe$/i.test(entry.name)) found.push(path);
    }
  };
  walk(base);
  if (!found.length) throw new Error("release-exe 下没有便携版，请用 --exe 指定");
  return found.sort((left, right) => statSync(right).mtimeMs - statSync(left).mtimeMs)[0];
}

const EXE = resolve(process.cwd(), arg("exe") ?? newestPortable());
const API_PORT = Number(arg("port", "18991"));
const CDP_PORT = Number(arg("cdp-port", "9444"));
const ROOT = resolve(process.cwd(), `.test-tmp/link-verify-${Date.now()}`);
const KEEP = argv.includes("--keep");
const EXPECT_OPEN = arg("expect", "github");

if (!existsSync(EXE)) {
  console.error(`找不到便携版：${EXE}`);
  process.exit(2);
}

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

async function waitForHealth(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`http://127.0.0.1:${API_PORT}/api/v1/health`, { signal: AbortSignal.timeout(1500) });
      if (response.ok) return await response.json();
    } catch {
      /* not up yet */
    }
    await sleep(400);
  }
  throw new Error("本地接口未就绪");
}

async function waitForTarget(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const targets = await fetch(`http://127.0.0.1:${CDP_PORT}/json/list`, { signal: AbortSignal.timeout(1500) }).then((r) => r.json());
      const page = targets.find((item) => item.type === "page" && item.webSocketDebuggerUrl);
      if (page) return page;
    } catch {
      /* devtools not ready */
    }
    await sleep(300);
  }
  throw new Error("没有找到 WebView DevTools target");
}

/** Evaluate one expression in the page and return its JSON value. */
function evaluate(target, expression) {
  return new Promise((resolveCommand, rejectCommand) => {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    const timer = setTimeout(() => {
      socket.close();
      rejectCommand(new Error("CDP evaluate timed out"));
    }, 15_000);
    socket.addEventListener("open", () => {
      socket.send(JSON.stringify({
        id: 1,
        method: "Runtime.evaluate",
        params: { expression, awaitPromise: true, returnByValue: true },
      }));
    });
    socket.addEventListener("message", (event) => {
      const message = JSON.parse(String(event.data));
      if (message.id !== 1) return;
      clearTimeout(timer);
      socket.close();
      if (message.error) rejectCommand(new Error(message.error.message));
      else if (message.result?.exceptionDetails) rejectCommand(new Error(message.result.exceptionDetails.text ?? "page exception"));
      else resolveCommand(message.result?.result?.value);
    });
    socket.addEventListener("error", () => {
      clearTimeout(timer);
      rejectCommand(new Error("DevTools WebSocket failed"));
    });
  });
}

mkdirSync(ROOT, { recursive: true });
writeFileSync(
  join(ROOT, "backend-config.json"),
  JSON.stringify({
    image_api_url: "", image_api_key: "", image_model: "gpt-image-2",
    video_api_url: "", video_api_key: "", video_model: "kling-video-v3",
    llm_api_url: "", llm_api_key: "", llm_model: "gemini-3.7-flash",
    output_dir: ROOT, source: "db",
  }, null, 2),
);

const child = spawn(EXE, [], {
  env: {
    ...process.env,
    IMAGE_CLIENT_DATA_DIR: ROOT,
    API_PORT: String(API_PORT),
    WEBVIEW2_USER_DATA_FOLDER: join(ROOT, "webview2"),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-address=127.0.0.1 --remote-debugging-port=${CDP_PORT}`,
  },
  stdio: "ignore",
});
console.log(`应用 pid=${child.pid} 数据目录=${ROOT}`);

let failed = false;
try {
  console.log(`健康检查: ${JSON.stringify(await waitForHealth(90_000))}`);
  const target = await waitForTarget(60_000);
  console.log(`DevTools target: ${target.url ?? target.title ?? "(page)"}`);

  const label = EXPECT_OPEN === "github" ? "GitHub 仓库" : "ZZone";
  const selector = EXPECT_OPEN === "github" ? '[aria-label="GitHub 仓库"]' : 'footer button[title*="ZZone"]';
  const clicked = await evaluate(
    target,
    `(() => { const el = document.querySelector(${JSON.stringify(selector)}); if (!el) return "missing"; el.click(); return "clicked"; })()`,
  );
  console.log(`按钮 ${selector} → ${clicked}`);
  if (clicked !== "clicked") failed = true;

  // The opener call is async; give the plugin a moment to resolve or reject.
  await sleep(4000);

  const logDir = join(ROOT, "logs");
  const logFile = existsSync(logDir) ? readdirSync(logDir).map((name) => join(logDir, name))[0] : undefined;
  const log = logFile ? readFileSync(logFile, "utf8") : "";
  const failures = log.split(/\r?\n/).filter((line) => line.includes("_open_failed") || line.includes("not allowed") || line.includes("forbidden"));
  const clickEvents = log.split(/\r?\n/).filter((line) => line.includes("ui.click") && line.includes("GitHub"));
  console.log(`\n点击事件: ${clickEvents.length ? clickEvents.at(-1).slice(0, 200) : "（无）"}`);
  if (failures.length) {
    console.log("打开失败日志:");
    for (const line of failures) console.log(`  ${line}`);
    failed = true;
  } else {
    console.log("✓ 日志中没有 open_failed / not allowed，说明 capability 放行且调用成功");
  }
} catch (error) {
  failed = true;
  console.log(`验证失败: ${error}`);
} finally {
  child.kill();
  await sleep(1500);
  if (!KEEP && !failed) rmSync(ROOT, { recursive: true, force: true });
  console.log(`\n进程已关闭。${KEEP || failed ? `数据目录保留：${ROOT}` : "临时数据目录已清理"}`);
  process.exit(failed ? 1 : 0);
}
