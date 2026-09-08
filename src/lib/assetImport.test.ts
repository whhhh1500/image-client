import { describe, expect, it } from "vitest";
import type { LibAsset } from "../store/useLibraryStore";
import { importEntryPrompt, libraryImportEntry, persistedGenerationPrompt } from "./assetImport";

function asset(kind: "text" | "image", params: Record<string, unknown>): LibAsset {
  return { asset: { id: `${kind}-a`, kind, path: `C:/${kind}` }, source: "测试资产", projectId: "project-a", params, createdAt: 1 };
}

describe("shared asset imports", () => {
  it("uses a text document's saved body rather than the prompt used to generate that document", () => {
    const document = asset("text", {
      text: "这是用户保存的正文",
      provenance: { generationInput: "把正文改写成摘要" },
    });
    expect(persistedGenerationPrompt(document)).toBe("这是用户保存的正文");
  });

  it("uses the immutable generation prompt for media", () => {
    const image = asset("image", { prompt: "旧的表单提示词", provenance: { generationInput: "实际生成图片的提示词" } });
    expect(importEntryPrompt(libraryImportEntry(image))).toBe("实际生成图片的提示词");
  });

  it("uses a canonical image's effective prompt instead of adjacent text context", () => {
    expect(importEntryPrompt({ entryType: "canonical_comic", readonly: true, sourceUri: "comic://page/1", projectId: "project-a", kind: "image", title: "第 1 页", text: "页面说明", effectivePrompt: "该图片实际使用的提示词", createdAt: 1 })).toBe("该图片实际使用的提示词");
    expect(importEntryPrompt({ entryType: "canonical_comic", readonly: true, sourceUri: "comic://page/old", projectId: "project-a", kind: "image", title: "旧图", text: "历史说明", createdAt: 1 })).toBeNull();
  });
});
