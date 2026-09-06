import { describe, expect, it, vi } from "vitest";
import type { LibAsset } from "../store/useLibraryStore";
import {
  createHistoryProvenance,
  historyProvenanceFromParams,
  inheritHistoryProvenance,
  legacyProvenanceFromAsset,
  snapshotAsset,
} from "./provenance";

describe("history provenance", () => {
  it("stores the original input and complete source material snapshot", () => {
    vi.spyOn(Date, "now").mockReturnValue(1234);
    const provenance = createHistoryProvenance({
      originalInput: "原始故事梗概",
      generationInput: "本次提交内容",
      contextSnapshot: "项目上下文",
      sourceMaterials: [{ kind: "text", label: "导入剧本", assetId: "script-1", text: "剧本正文" }],
      parentAssetIds: ["script-1"],
      revision: { type: "generated" },
    });
    expect(provenance).toMatchObject({
      schemaVersion: 1,
      originalInput: "原始故事梗概",
      generationInput: "本次提交内容",
      recordedAt: 1234,
      parentAssetIds: ["script-1"],
    });
    expect(historyProvenanceFromParams({ provenance })?.sourceMaterials[0].text).toBe("剧本正文");
    vi.restoreAllMocks();
  });

  it("preserves the first-generation source when a new version is edited", () => {
    const original = createHistoryProvenance({
      originalInput: "最初输入",
      sourceMaterials: [{ kind: "image", label: "角色参考图", assetId: "image-1", path: "/image.png" }],
      parentAssetIds: ["image-1"],
    });
    const edited = inheritHistoryProvenance(original, {
      revision: { type: "ai_optimized", instruction: "强化节奏", basedOnAssetId: "doc-v1" },
    });
    expect(edited.originalInput).toBe("最初输入");
    expect(edited.sourceMaterials[0].assetId).toBe("image-1");
    expect(edited.revision).toEqual({ type: "ai_optimized", instruction: "强化节奏", basedOnAssetId: "doc-v1" });
  });

  it("does not duplicate the same referenced asset", () => {
    const provenance = createHistoryProvenance({
      sourceMaterials: [
        { kind: "image", label: "自动识别参考图", assetId: "image-1", path: "/image.png" },
        { kind: "image", label: "手动选择参考图", assetId: "image-1", path: "/image.png" },
      ],
    });
    expect(provenance.sourceMaterials).toHaveLength(1);
  });

  it("recovers prompt and reference path from old media history", () => {
    const asset: LibAsset = {
      asset: { id: "old-image", kind: "image", path: "/output.png" },
      source: "图生图",
      params: { prompt: "人物站在雨中", referencePath: "/reference.png" },
      createdAt: 1,
    };
    const recovered = legacyProvenanceFromAsset(asset)!;
    expect(recovered.originalInput).toBe("人物站在雨中");
    expect(recovered.sourceMaterials[0].path).toBe("/reference.png");
  });

  it("snapshots asset identity, content and path", () => {
    const asset: LibAsset = {
      asset: { id: "storyboard-1", kind: "text", path: "/storyboard.md" },
      source: "分镜",
      model: "model-a",
      createdAt: 1,
    };
    expect(snapshotAsset(asset, "完整分镜")).toEqual({
      kind: "text",
      label: "分镜",
      assetId: "storyboard-1",
      source: "分镜",
      path: "/storyboard.md",
      text: "完整分镜",
      model: "model-a",
    });
  });

  it("preserves comic run and attempt provenance for the first generated-asset persistence", () => {
    const provenance = createHistoryProvenance({
      comicGeneration: {
        comicRunId: "cprun_1",
        generationAttemptId: "attempt_1",
        kind: "page",
        comicProjectId: "comic_1",
        pageNo: 3,
        role: "full_page",
      },
    });
    expect(historyProvenanceFromParams({ provenance })?.comicGeneration).toEqual({
      comicRunId: "cprun_1",
      generationAttemptId: "attempt_1",
      kind: "page",
      comicProjectId: "comic_1",
      pageNo: 3,
      role: "full_page",
    });
  });

});
