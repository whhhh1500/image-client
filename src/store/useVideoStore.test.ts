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

  it("preserves a valid local image identity and treats changes as production-manifest edits", () => {
    const local = { assetId: "image-a", path: "D:/image-a.png", label: "角色设定" };
    const params = normalizeVideoParams({
      shots: [{ id: "shot", shotNo: 1, prompt: "镜头", durationS: 3, referenceStrategy: "first_frame", referenceLocalImages: [local] }],
      model: "model", aspectRatio: "16:9", resolution: "720p", storyboardSourceAssetId: "storyboard",
      productionManifest: { storyboardAssetId: "storyboard", approvedModel: "model", approvedAspectRatio: "16:9", approvedResolution: "720p", approvedAt: 1, shots: [{ shotNo: 1, prompt: "镜头", durationS: 3, referenceStrategy: "first_frame", referenceAssetIds: [], referenceImages: [], referenceVideos: [], referenceLocalImages: [local] }] },
    });
    expect(params.shots[0].referenceLocalImages).toEqual([local]);
    expect(productionManifestMismatch(params)).toBeNull();
    expect(productionManifestMismatch({ ...params, shots: [{ ...params.shots[0], referenceLocalImages: [{ ...local, path: "D:/changed.png" }] }] })).toContain("本地图片参考");
  });
});
