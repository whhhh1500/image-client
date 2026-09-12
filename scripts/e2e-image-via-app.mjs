#!/usr/bin/env node
// 端到端验证：用打包好的应用自身的 REST 路由，把「应用真实代码路径」打到真网关。
//
//   $env:IMAGE_CLIENT_PROBE_KEY = "sk-..."
//   node scripts/e2e-image-via-app.mjs [--exe <便携版路径>]
//
// 流程：隔离数据目录写入 backend-config.json → 启动便携版（独占端口）→
// 调 POST /api/v1/media/images/generations → 校验图片与日志 → 关闭进程。

import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { resolve, join } from "node:path";

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

const BASE = (arg("base", "https://api.cc1500.top")).replace(/\/+$/, "");
const KEY = arg("key") ?? process.env.IMAGE_CLIENT_PROBE_KEY ?? "";
const MODEL = arg("model", "grok-imagine-image");
const COUNT = Number(arg("n", "1"));
const SIZE = arg("size", "1024x1536 (2:3)");
const QUALITY = arg("quality", "high");
const BACKGROUND = arg("background", "");
/** Pad the prompt to this many characters, to prove no length rule blocks it. */
const PROMPT_CHARS = Number(arg("prompt-chars", "0"));
const PORT = Number(arg("port", "18999"));
const EXE = resolve(process.cwd(), arg("exe") ?? newestPortable());
const ROOT = resolve(process.cwd(), `.test-tmp/app-e2e-${Date.now()}`);
const KEEP = argv.includes("--keep");

if (!KEY) {
  console.error("缺少密钥：请设置 IMAGE_CLIENT_PROBE_KEY 环境变量或传 --key");
  process.exit(2);
}
if (!existsSync(EXE)) {
  console.error(`找不到便携版：${EXE}`);
  process.exit(2);
}

/** PNG/JPEG dimensions so the app's own output can be checked. */
function imageInfo(bytes) {
  if (bytes.subarray(0, 8).toString("hex") === "89504e470d0a1a0a") {
    return `png ${bytes.readUInt32BE(16)}x${bytes.readUInt32BE(20)}`;
  }
  if (bytes[0] === 0xff && bytes[1] === 0xd8) {
    let offset = 2;
    while (offset + 9 < bytes.length) {
      if (bytes[offset] !== 0xff) { offset += 1; continue; }
      const marker = bytes[offset + 1];
      const length = bytes.readUInt16BE(offset + 2);
      if (marker >= 0xc0 && marker <= 0xcf && ![0xc4, 0xc8, 0xcc].includes(marker)) {
        return `jpeg ${bytes.readUInt16BE(offset + 7)}x${bytes.readUInt16BE(offset + 5)}`;
      }
      offset += 2 + length;
    }
    return "jpeg";
  }
  return `unknown (${bytes.length} bytes)`;
}

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

async function waitForHealth(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`http://127.0.0.1:${PORT}/api/v1/health`, {
        signal: AbortSignal.timeout(2000),
      });
      if (response.ok) return await response.json();
    } catch {
      /* not up yet */
    }
    await sleep(500);
  }
  throw new Error(`${timeoutMs} ms 内没有等到本地接口就绪`);
}

mkdirSync(ROOT, { recursive: true });
writeFileSync(
  join(ROOT, "backend-config.json"),
  JSON.stringify(
    {
      image_api_url: `${BASE}/v1/images/generations`,
      image_api_key: KEY,
      image_model: MODEL,
      video_api_url: "",
      video_api_key: "",
      video_model: "kling-video-v3",
      llm_api_url: "",
      llm_api_key: "",
      llm_model: "gemini-3.7-flash",
      output_dir: ROOT,
      source: "api",
    },
    null,
    2,
  ),
);

console.log(`应用: ${EXE}`);
console.log(`数据目录: ${ROOT}`);
console.log(`网关: ${BASE}  模型: ${MODEL}  端口: ${PORT}`);

const child = spawn(EXE, [], {
  env: { ...process.env, IMAGE_CLIENT_DATA_DIR: ROOT, API_PORT: String(PORT) },
  stdio: "ignore",
  detached: false,
});
console.log(`已启动 pid=${child.pid}`);

let failed = false;
try {
  const health = await waitForHealth(90_000);
  console.log(`健康检查: ${JSON.stringify(health)}`);

  const basePrompt = "一只戴红色围巾的橘猫坐在木桌上，柔和自然光，竖版构图";
  const prompt = PROMPT_CHARS > 0
    ? basePrompt + "，细节：" + "柔软毛发与木质纹理".repeat(Math.ceil(PROMPT_CHARS / 9)).slice(0, Math.max(PROMPT_CHARS - basePrompt.length - 4, 0))
    : basePrompt;
  const body = {
    prompt,
    model: MODEL,
    size: SIZE,
    quality: QUALITY,
    n: COUNT,
    // Omitted unless asked for: the app itself does not send background when
    // the form stays on "auto".
    ...(BACKGROUND ? { background: BACKGROUND } : {}),
  };
  console.log(`提示词长度: ${[...prompt].length} 字符`);
  console.log(`\nPOST /api/v1/media/images/generations\n  ${JSON.stringify(body)}`);
  const started = Date.now();
  const response = await fetch(`http://127.0.0.1:${PORT}/api/v1/media/images/generations`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(300_000),
  });
  const text = await response.text();
  console.log(`应用返回 HTTP ${response.status}（${Date.now() - started} ms）`);
  console.log(`  ${text.slice(0, 400)}`);
  if (!response.ok) failed = true;

  const assets = response.ok ? JSON.parse(text).assets ?? [] : [];
  if (response.ok && assets.length !== COUNT) {
    console.log(`  ⚠ 请求 ${COUNT} 张但返回 ${assets.length} 张`);
  }
  for (const [index, asset] of assets.entries()) {
    if (!existsSync(asset.path)) {
      console.log(`  ✗ 资产文件缺失: ${asset.path}`);
      failed = true;
      continue;
    }
    const info = imageInfo(readFileSync(asset.path));
    console.log(`  ✓ 已保存: ${asset.path} — ${info}`);
    const extension = info.startsWith("png") ? "png" : info.startsWith("jpeg") ? "jpg" : "bin";
    const keep = resolve(process.cwd(), ".test-tmp/image-params-probe", `app-e2e-${MODEL}-${index + 1}.${extension}`);
    mkdirSync(resolve(process.cwd(), ".test-tmp/image-params-probe"), { recursive: true });
    copyFileSync(asset.path, keep);
    console.log(`  ✓ 副本: ${keep}`);
  }

  const logDir = join(ROOT, "logs");
  const logFile = existsSync(logDir) ? readdirSync(logDir).map((name) => join(logDir, name))[0] : undefined;
  if (logFile) {
    const lines = readFileSync(logFile, "utf8").split(/\r?\n/).filter(Boolean);
    const requestStart = lines.filter((line) => line.includes("image.request.start")).at(-1);
    const requestEnd = lines.filter((line) => line.includes("image.request.end")).at(-1);
    console.log("\n应用日志（图片请求）:");
    for (const line of [requestStart, requestEnd]) {
      if (!line) continue;
      const parsed = JSON.parse(line);
      console.log(`  ${parsed.event}: ${JSON.stringify(parsed.fields)}`);
    }
    if (/grok/i.test(MODEL) && requestStart && !requestStart.includes("\"aspectRatio\":\"") ) {
      console.log("  ⚠ Grok 模型缺少 aspectRatio，请人工确认");
    }
    if (!/grok/i.test(MODEL) && requestStart?.includes("\"aspectRatio\":\"")) {
      console.log("  ⚠ 非 Grok 模型不应带 aspectRatio，请人工确认");
    }
  }
} catch (error) {
  failed = true;
  console.log(`\n失败: ${error}`);
} finally {
  child.kill();
  await sleep(1500);
  if (!KEEP && !failed) rmSync(ROOT, { recursive: true, force: true });
  console.log(`\n进程已关闭。${KEEP || failed ? `数据目录保留：${ROOT}` : "临时数据目录已清理"}`);
  process.exit(failed ? 1 : 0);
}
