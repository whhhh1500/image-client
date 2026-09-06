import { describe, expect, it, vi } from "vitest";

vi.mock("./ipc", () => ({
  llmChat: vi.fn(async () => "```text\n亚麻桌布上的红苹果，棚拍静物，柔光，高清。\n```"),
}));

import { IMAGE_PROMPT_ENGINEER, optimizeImagePrompt } from "./optimizePrompt";
import { llmChat } from "./ipc";

describe("optimizeImagePrompt", () => {
  it("asks the image prompt engineer and unwraps fenced output", async () => {
    const result = await optimizeImagePrompt({
      prompt: "苹果",
      guidance: ["锁定光线"],
      pitfalls: ["避免乱码文字"],
    });
    expect(result).toBe("亚麻桌布上的红苹果，棚拍静物，柔光，高清。");
    expect(IMAGE_PROMPT_ENGINEER).toContain("只用中文写最终提示词");
    expect(vi.mocked(llmChat)).toHaveBeenCalledWith(
      IMAGE_PROMPT_ENGINEER,
      expect.stringContaining("当前提示词"),
      undefined,
    );
  });
});
