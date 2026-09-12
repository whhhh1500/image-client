#!/usr/bin/env node
// 网关生图参数探针：验证 Grok 图像模型的请求参数契约。
//
// 用法（密钥只从环境变量读取，不写入仓库）：
//   $env:IMAGE_CLIENT_PROBE_KEY = "sk-..."
//   node scripts/probe-image-params.mjs --cases=models,new-shape,legacy-size
//
// 每个用例都是一次真实计费的 POST，用 --cases 精确选择要跑的组合。

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const argv = process.argv.slice(2);
const arg = (name, fallback) => {
  const inline = argv.find((value) => value.startsWith(`--${name}=`));
  if (inline) return inline.slice(name.length + 3);
  const index = argv.indexOf(`--${name}`);
  return index >= 0 ? argv[index + 1] : fallback;
};

const BASE = (arg("base", "https://api.cc1500.top")).replace(/\/+$/, "");
const KEY = arg("key") ?? process.env.IMAGE_CLIENT_PROBE_KEY ?? "";
const OUT_DIR = resolve(process.cwd(), arg("out", ".test-tmp/image-params-probe"));
const REQUESTED = (arg("cases", "models,new-shape,legacy-size,prompt-limit,edits") ?? "")
  .split(",")
  .map((value) => value.trim())
  .filter(Boolean);
const TIMEOUT_MS = Number(arg("timeout", "240000"));
/** Image count for the batch-capable cases. */
const N = Math.min(Math.max(Number(arg("n", "1")) || 1, 1), 10);

if (!KEY) {
  console.error("缺少密钥：请设置 IMAGE_CLIENT_PROBE_KEY 环境变量或传 --key（不会写入文件）");
  process.exit(2);
}

const IMAGE_ENDPOINT = `${BASE}/v1/images/generations`;
const EDITS_ENDPOINT = `${BASE}/v1/images/edits`;
const MODELS_ENDPOINT = `${BASE}/v1/models`;

/** PNG/JPEG/WebP dimensions, enough to verify what the gateway actually produced. */
function imageInfo(bytes) {
  if (bytes.length > 8 && bytes.subarray(0, 8).toString("hex") === "89504e470d0a1a0a") {
    return {
      format: "png",
      width: bytes.readUInt32BE(16),
      height: bytes.readUInt32BE(20),
    };
  }
  if (bytes.length > 4 && bytes[0] === 0xff && bytes[1] === 0xd8) {
    let offset = 2;
    while (offset + 9 < bytes.length) {
      if (bytes[offset] !== 0xff) {
        offset += 1;
        continue;
      }
      const marker = bytes[offset + 1];
      const length = bytes.readUInt16BE(offset + 2);
      if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
        return {
          format: "jpeg",
          height: bytes.readUInt16BE(offset + 5),
          width: bytes.readUInt16BE(offset + 7),
        };
      }
      offset += 2 + length;
    }
    return { format: "jpeg" };
  }
  if (bytes.length > 30 && bytes.subarray(0, 4).toString() === "RIFF" && bytes.subarray(8, 12).toString() === "WEBP") {
    return { format: "webp" };
  }
  return { format: "unknown" };
}

/** Summarize a response without dumping megabytes of base64 into the console. */
function summarize(payload) {
  const items = Array.isArray(payload?.data) ? payload.data : [];
  const first = items[0];
  if (!first) return { dataLength: items.length, keys: Object.keys(payload ?? {}) };
  const summary = { dataLength: items.length, itemKeys: Object.keys(first), usage: payload.usage ?? null };
  if (typeof first.b64_json === "string") {
    const bytes = Buffer.from(first.b64_json, "base64");
    summary.mime = "b64_json";
    summary.bytes = bytes.length;
    Object.assign(summary, imageInfo(bytes));
    summary.head = `data:image/${summary.format};base64,${first.b64_json.slice(0, 16)}…`;
    summary.__bytes = bytes;
  } else if (typeof first.url === "string") {
    summary.mime = "url";
    summary.url = first.url;
  } else if (typeof first.image_url === "string") {
    summary.mime = "image_url";
    summary.url = first.image_url;
  }
  if (typeof first.revised_prompt === "string") summary.revisedPrompt = true;
  return summary;
}

async function postJson(url, body, label) {
  const started = Date.now();
  const response = await fetch(url, {
    method: "POST",
    headers: { Authorization: `Bearer ${KEY}`, "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(TIMEOUT_MS),
  });
  const text = await response.text();
  let payload;
  try {
    payload = JSON.parse(text);
  } catch {
    payload = { __raw: text.slice(0, 400) };
  }
  return { label, status: response.status, ms: Date.now() - started, payload, request: body };
}

async function postMultipart(url, fields, file, label) {
  const form = new FormData();
  for (const [name, value] of Object.entries(fields)) form.append(name, String(value));
  form.append("image", new Blob([file.bytes], { type: "image/png" }), file.name);
  const started = Date.now();
  const response = await fetch(url, {
    method: "POST",
    headers: { Authorization: `Bearer ${KEY}` },
    body: form,
    signal: AbortSignal.timeout(TIMEOUT_MS),
  });
  const text = await response.text();
  let payload;
  try {
    payload = JSON.parse(text);
  } catch {
    payload = { __raw: text.slice(0, 400) };
  }
  return { label, status: response.status, ms: Date.now() - started, payload, request: { ...fields, image: file.name } };
}

function report(result, note) {
  const summary = result.status >= 200 && result.status < 300 ? summarize(result.payload) : null;
  const error = result.status >= 300 ? JSON.stringify(result.payload).slice(0, 300) : null;
  console.log(`\n=== ${result.label} — HTTP ${result.status} (${result.ms} ms)`);
  console.log(`  请求: ${JSON.stringify(result.request)}`);
  if (note) console.log(`  说明: ${note}`);
  if (summary) {
    const { __bytes, ...rest } = summary;
    console.log(`  响应: ${JSON.stringify(rest)}`);
    if (__bytes) {
      const file = resolve(OUT_DIR, `${result.label.replace(/[^\w.-]+/g, "_")}.${summary.format}`);
      writeFileSync(file, __bytes);
      console.log(`  已保存: ${file}`);
    }
  } else if (error) {
    console.log(`  错误: ${error}`);
  }
  return { label: result.label, status: result.status, summary, error };
}

async function listModels() {
  const response = await fetch(MODELS_ENDPOINT, {
    headers: { Authorization: `Bearer ${KEY}` },
    signal: AbortSignal.timeout(30_000),
  });
  const text = await response.text();
  let payload;
  try {
    payload = JSON.parse(text);
  } catch {
    payload = { __raw: text.slice(0, 200) };
  }
  if (!response.ok) {
    console.log(`\n=== models — HTTP ${response.status}\n  ${JSON.stringify(payload).slice(0, 300)}`);
    return [];
  }
  const ids = (payload.data ?? payload.models ?? [])
    .map((item) => (typeof item === "string" ? item : item.id ?? item.name))
    .filter(Boolean);
  const grok = ids.filter((id) => /grok/i.test(id) && /image|imagine/i.test(id));
  console.log(`\n=== models — HTTP 200，共 ${ids.length} 个`);
  console.log(`  图像相关 Grok: ${grok.length ? grok.join(", ") : "（无）"}`);
  console.log(`  全部 Grok: ${ids.filter((id) => /grok/i.test(id)).join(", ") || "（无）"}`);
  const others = ids.filter((id) => /image/i.test(id) && !/grok/i.test(id)).slice(0, 8);
  console.log(`  其他图像模型(前 8): ${others.join(", ") || "（无）"}`);
  return ids;
}

const PROMPT = "一只戴红色围巾的橘猫坐在木桌上，柔和自然光，方形构图";
const LONG_PROMPT = "猫".repeat(1001);

async function main() {
  mkdirSync(OUT_DIR, { recursive: true });
  console.log(`网关: ${BASE}`);
  console.log(`用例: ${REQUESTED.join(", ")}`);
  console.log(`输出: ${OUT_DIR}`);

  // 目录查询是免费 GET，始终先跑一次，用来发现真实可用的模型 id。
  const ids = await listModels();
  const grokModel =
    arg("model") ??
    ids.find((id) => /grok/i.test(id) && /image|imagine/i.test(id)) ??
    "grok-3-image";
  const otherModel = ids.find((id) => /image/i.test(id) && !/grok/i.test(id));

  console.log(`\nGrok 模型: ${grokModel}`);
  const results = [];
  const run = async (name, fn, note) => {
    if (!REQUESTED.includes(name)) return;
    try {
      results.push(report(await fn(), note));
    } catch (error) {
      console.log(`\n=== ${name} — 请求失败: ${error}`);
      results.push({ label: name, status: 0, error: String(error) });
    }
  };

  // 新契约：aspect_ratio + resolution + response_format（应用现在发的形状）
  await run(
    "new-shape",
    () =>
      postJson(
        IMAGE_ENDPOINT,
        {
          model: grokModel,
          prompt: PROMPT,
          n: N,
          aspect_ratio: "1:1",
          resolution: "1k",
          response_format: "b64_json",
          quality: "high",
        },
        "new-shape",
      ),
    "应用新形状：aspect_ratio + resolution + response_format + quality",
  );

  // 旧契约：OpenAI 风格 size / background（优化前应用发的形状）
  await run(
    "legacy-size",
    () =>
      postJson(
        IMAGE_ENDPOINT,
        { model: grokModel, prompt: PROMPT, n: 1, size: "1024x1024", quality: "high" },
        "legacy-size",
      ),
    "旧形状：OpenAI size=1024x1024（Grok 支持列表里没有该尺寸）",
  );

  await run(
    "legacy-background",
    () =>
      postJson(
        IMAGE_ENDPOINT,
        {
          model: grokModel,
          prompt: PROMPT,
          n: 1,
          size: "1024x1024",
          quality: "high",
          background: "transparent",
        },
        "legacy-background",
      ),
    "旧形状：额外带 background（Grok 契约没有该参数）",
  );

  await run(
    "ratio-with-size",
    () =>
      postJson(
        IMAGE_ENDPOINT,
        {
          model: grokModel,
          prompt: PROMPT,
          n: 1,
          aspect_ratio: "16:9",
          resolution: "1k",
          size: "1280x720",
          response_format: "b64_json",
        },
        "ratio-with-size",
      ),
    "同时发送 aspect_ratio 与官方允许的 size=1280x720，看是否冲突",
  );

  await run(
    "no-response-format",
    () =>
      postJson(
        IMAGE_ENDPOINT,
        { model: grokModel, prompt: PROMPT, n: 1, aspect_ratio: "9:16", resolution: "2k" },
        "no-response-format",
      ),
    "不带 response_format 时默认返回什么（2k 竖版）",
  );

  await run(
    "prompt-limit",
    () =>
      postJson(
        IMAGE_ENDPOINT,
        {
          model: grokModel,
          prompt: LONG_PROMPT,
          n: 1,
          aspect_ratio: "1:1",
          resolution: "1k",
          response_format: "b64_json",
        },
        "prompt-limit",
      ),
    "1001 字提示词：确认网关是否按 1000 字上限拒绝",
  );

  await run(
    "edits",
    () => {
      const icon = readFileSync(resolve(process.cwd(), "src-tauri/icons/32x32.png"));
      return postMultipart(
        EDITS_ENDPOINT,
        {
          model: grokModel,
          prompt: "把这张图改成蓝色背景",
          n: 1,
          aspect_ratio: "1:1",
          resolution: "1k",
        },
        { bytes: icon, name: "reference.png" },
        "edits",
      );
    },
    "图生图 /v1/images/edits：Grok 模型是否接受 aspect_ratio + resolution 表单字段",
  );

  await run(
    "ratio-16x9",
    () =>
      postJson(
        IMAGE_ENDPOINT,
        {
          model: grokModel,
          prompt: PROMPT,
          n: 1,
          aspect_ratio: "16:9",
          resolution: "1k",
          response_format: "b64_json",
        },
        "ratio-16x9",
      ),
    "只用 aspect_ratio=16:9 + resolution=1k，看实际输出尺寸",
  );

  await run(
    "ratio-2x3-2k",
    () =>
      postJson(
        IMAGE_ENDPOINT,
        {
          model: grokModel,
          prompt: PROMPT,
          n: 1,
          aspect_ratio: "2:3",
          resolution: "2k",
          response_format: "b64_json",
        },
        "ratio-2x3-2k",
      ),
    "aspect_ratio=2:3 + resolution=2k（漫画竖版场景）",
  );

  await run(
    "legacy-portrait",
    () =>
      postJson(
        IMAGE_ENDPOINT,
        { model: grokModel, prompt: PROMPT, n: 1, size: "1024x1536", quality: "high" },
        "legacy-portrait",
      ),
    "旧形状的竖版：size=1024x1536 不在 Grok 支持列表，看是否被忽略成方图",
  );

  await run(
    "non-grok",
    () =>
      postJson(
        IMAGE_ENDPOINT,
        {
          model: otherModel ?? "gpt-image-2",
          prompt: PROMPT,
          n: 1,
          size: "1024x1024",
          quality: "high",
        },
        "non-grok",
      ),
    "对照：非 Grok 模型仍按 size/quality 提交",
  );

  console.log("\n\n================ 汇总 ================");
  for (const result of results) {
    const detail = result.summary
      ? `${result.summary.format ?? result.summary.mime} ${result.summary.width ?? "?"}x${result.summary.height ?? "?"}`
      : (result.error ?? "").slice(0, 120);
    console.log(`${String(result.status).padStart(3)}  ${result.label.padEnd(20)} ${detail}`);
  }
}

await main();
