import { describe, expect, it } from "vitest";
import { normalizeVideoParams, productionManifestMismatch } from "./useVideoStore";

describe("normalizeVideoParams", () => {
  it("normalizes current per-shot generation settings", () => {
    const params = normalizeVideoParams({
      shots: [
        { id: "a", shotNo: 9, prompt: " 第一镜 ", durationS: 3 },
        { id: "b", shotNo: 12, prompt: "第二镜", durationS: 8 },
      ],
      mode: "reference",
      images: [" https://example.com/hero.png "],
      storyboardSourceAssetId: "storyboard-1",
    });
    expect(params.shots).toEqual([
      { id: "a", shotNo: 1, prompt: "第一镜", durationS: 3 },
      { id: "b", shotNo: 2, prompt: "第二镜", durationS: 8 },
    ]);
    expect(params.images).toEqual(["https://example.com/hero.png"]);
    expect(params.storyboardSourceAssetId).toBe("storyboard-1");
  });

  it("does not infer shots from removed fields", () => {
    expect(normalizeVideoParams({}).shots).toHaveLength(1);
  });

  it("detects edits that invalidate an approved production manifest", () => {
    const params = normalizeVideoParams({
      shots: [{ id: "a", shotNo: 1, prompt: "镜头", durationS: 3, referenceStrategy: "text" }],
      model: "model-a", aspectRatio: "9:16", storyboardSourceAssetId: "storyboard-a",
      resolution: "720p",
      productionManifest: { storyboardAssetId: "storyboard-a", approvedModel: "model-a", approvedAspectRatio: "9:16", approvedResolution: "720p", approvedAt: 1, shots: [{ shotNo: 1, prompt: "镜头", durationS: 3, referenceStrategy: "text", referenceAssetIds: [], referenceImages: [], referenceVideos: [] }] },
    });
    expect(productionManifestMismatch(params)).toBeNull();
    expect(productionManifestMismatch({ ...params, aspectRatio: "16:9" })).toContain("画幅已从");
    expect(productionManifestMismatch({ ...params, resolution: "480p" })).toContain("分辨率已从");
    expect(productionManifestMismatch({ ...params, shots: [{ ...params.shots[0], prompt: "已修改" }] })).toContain("第 1 镜");
  });
});
