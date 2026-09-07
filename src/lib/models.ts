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
