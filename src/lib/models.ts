// 模型清单以 .env 注释为准（可用模型）。
export const IMAGE_MODELS = [
  "gpt-image-2",
  "gemini-3.1-flash-image",
  "gemini-3.1-flash-image-preview",
  "gemini-2.5-flash-image",
  "gemini-2.5-flash-image-preview",
  "gemini-3-pro-image",
  "gemini-3-pro-image-preview",
  "grok-imagine-image",
];

export const VIDEO_MODELS = [
  "video-ds-2.5",
  "video-ds-2.5-480",
  "grok-imagine-video",
  "grok-imagine-video-1.5-preview",
  "kling-video-v3",
  "kling-video-v3-omni",
  "kling-video-v3-turbo",
  "seedance2.5",
  "minimax-h3",
  "minimax-h3-2k",
  "minimax-h3-4k",
  "drama-video-v2",
  "drama-video-v2-fast",
  "as-sd2.0-fast",
  "video-ds-2.0",
  "video-ds-2.0-fast",
  "wan3-720p",
];

// 文本大模型（生产 Agent 用），默认 gemini-3.7-flash。
export const LLM_MODELS = [
  "gemini-3.7-flash",
  "gemini-3.6-flash",
  "gemini-3.5-flash",
  "gemini-3-flash",
  "gemini-2.5-flash",
  "gemini-2.5-flash-lite",
  "gemini-3.1-flash-lite",
  "gemini-2.5-pro",
];

/**
 * Grok 图像模型不使用 OpenAI 的 `size` 契约：后端会改发
 * `aspect_ratio` + `resolution`（1k/2k），并忽略 `background`；
 * 若网关拒绝该契约，后端会自动用 OpenAI 参数重试一次。
 *
 * 判定规则与后端 `gateway::is_grok_image_model` 保持一致：
 * 供应商段为 `grok`、`grok-*` 或 `grok<数字>*`（`grokking-image` 不算）。
 */
export function isGrokImageModel(model: string): boolean {
  return model
    .trim()
    .toLowerCase()
    .split(/[/:@]/)
    .some((segment) => {
      if (segment === "grok") return true;
      if (!segment.startsWith("grok")) return false;
      const rest = segment.slice(4);
      return /^[-_]/.test(rest) || /^\d/.test(rest);
    });
}

/** 单次请求最多生成的图片数量（与后端 gateway::MAX_IMAGE_BATCH 一致）。 */
export const MAX_IMAGE_COUNT = 4;
