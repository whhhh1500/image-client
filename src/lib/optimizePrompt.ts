import { stripThinking } from "./aiOutput";
import { llmChat } from "./ipc";

export const IMAGE_PROMPT_ENGINEER = `你是面向 gpt-image-2 / 兼容图像网关的中文提示词工程师。把用户意图整理成可直接用于文生图的最终中文 prompt。

规则：
- 只用中文写最终提示词。不要整段改写成英文。
- 画面里要出现的文字（标题、招牌、UI、海报文案）必须按用户指定语言原样写出，并明确「画面文字必须准确、清晰、不乱码」。
- 保留用户指定的主体、构图、品牌、约束；不要发明用户没提的剧情。
- 填实模板里的 [占位符]；用户没给的改成具体、可拍的中文默认，不要留下方括号。
- 写清：主体、场景、构图、光线、材质、色彩、镜头/比例、风格，以及不要出现的内容。
- 摄影/材质专有名词（如 50mm、f/1.4、rim light、Kodak Portra、Unreal Engine）可夹在中文句子里原样保留，不要把整段描述改回英文。
- 若提供了模板 guidance / pitfalls，必须遵守：用 guidance，避开 pitfalls。
- 只输出最终中文 prompt 正文，不要解释、不要 markdown、不要代码块。`;

function unwrapPrompt(text: string): string {
  const cleaned = stripThinking(text).trim();
  const fenced = /```(?:[a-zA-Z0-9_-]+)?\s*([\s\S]*?)```/.exec(cleaned);
  return (fenced?.[1] ?? cleaned).trim();
}

export async function optimizeImagePrompt(input: {
  prompt: string;
  userIntent?: string;
  guidance?: string[];
  pitfalls?: string[];
  model?: string;
}): Promise<string> {
  const prompt = input.prompt.trim();
  if (!prompt) throw new Error("没有可优化的提示词");
  const parts = [`当前提示词：\n${prompt}`];
  if (input.userIntent?.trim()) parts.push(`用户补充：\n${input.userIntent.trim()}`);
  if (input.guidance?.length) parts.push(`模板建议：\n- ${input.guidance.join("\n- ")}`);
  if (input.pitfalls?.length) parts.push(`必须避开：\n- ${input.pitfalls.join("\n- ")}`);
  parts.push("请输出可直接用于文生图的中文提示词。");
  const result = await llmChat(IMAGE_PROMPT_ENGINEER, parts.join("\n\n"), input.model);
  const optimized = unwrapPrompt(result);
  if (!optimized) throw new Error("模型没有返回可用提示词");
  return optimized;
}
