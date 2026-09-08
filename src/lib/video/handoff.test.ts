import { describe, expect, it, vi } from "vitest";
import type { LibAsset } from "../../store/useLibraryStore";
import { createEmptyStoryboardShot } from "./storyboard";
import { buildReviewedVideoHandoff } from "./handoff";

describe("reviewed storyboard handoff", () => {
  it("locks approved model/aspect and resolves per-shot media without leaking references", () => {
    vi.spyOn(Date, "now").mockReturnValue(123);
    const first = createEmptyStoryboardShot([]);
    first.referenceStrategy = "first_frame";
    first.referenceAssetIds = ["image-a"];
    const second = { ...createEmptyStoryboardShot([first]), referenceStrategy: "text" as const, referenceAssetIds: [] };
    const source: LibAsset = { asset: { id: "storyboard-a", kind: "text", path: "storyboard.md" }, source: "分镜", projectId: "p", createdAt: 1 };
    const image: LibAsset = { asset: { id: "image-a", kind: "image", path: "https://example.com/a.png" }, source: "参考图", projectId: "p", createdAt: 1 };
    const result = buildReviewedVideoHandoff([first, second], [source, image], source, { anchorAssetId: "anchors-a", qcAssetId: "qc-a", approvedModel: "model-a", approvedAspectRatio: "9:16", approvedResolution: "720p" });
    expect(result).toMatchObject({ model: "model-a", aspectRatio: "9:16", resolution: "720p", storyboardSourceAssetId: "storyboard-a" });
    expect(result.shots?.[0]).toMatchObject({ referenceStrategy: "first_frame", referenceAssetIds: ["image-a"], referenceImages: ["https://example.com/a.png"] });
    expect(result.shots?.[1]).toMatchObject({ referenceStrategy: "text", referenceAssetIds: [], referenceImages: [] });
    expect(result.productionManifest).toMatchObject({ storyboardAssetId: "storyboard-a", anchorAssetId: "anchors-a", qcAssetId: "qc-a", approvedModel: "model-a", approvedAspectRatio: "9:16", approvedResolution: "720p", approvedAt: 123 });
    vi.restoreAllMocks();
  });

  it("keeps same-project local images as local identities while retaining only hosted URLs for provider media", () => {
    const first = createEmptyStoryboardShot([]);
    first.referenceStrategy = "reference";
    first.referenceAssetIds = ["image-local", "image-hosted", "foreign-image", "video-local", "video-hosted"];
    const source: LibAsset = { asset: { id: "storyboard-a", kind: "text", path: "storyboard.md" }, source: "分镜", projectId: "p", createdAt: 1 };
    const assets: LibAsset[] = [
      source,
      { asset: { id: "image-local", kind: "image", path: "C:/private.png" }, source: "本地图片", projectId: "p", createdAt: 1 },
      { asset: { id: "image-hosted", kind: "image", path: "https://cdn.example/image.png" }, source: "托管图片", projectId: "p", createdAt: 1 },
      { asset: { id: "foreign-image", kind: "image", path: "C:/other.png" }, source: "其他项目", projectId: "other", createdAt: 1 },
      { asset: { id: "video-local", kind: "video", path: "C:/private.mp4" }, source: "本地视频", projectId: "p", createdAt: 1 },
      { asset: { id: "video-hosted", kind: "video", path: "https://cdn.example/video.mp4" }, source: "托管视频", projectId: "p", createdAt: 1 },
    ];
    const result = buildReviewedVideoHandoff([first], assets, source, { approvedModel: "model", approvedAspectRatio: "9:16", approvedResolution: "720p" });
    expect(result.shots?.[0]).toMatchObject({
      referenceAssetIds: ["image-local", "image-hosted", "foreign-image", "video-local", "video-hosted"],
      referenceImages: ["https://cdn.example/image.png"],
      referenceLocalImages: [{ assetId: "image-local", path: "C:/private.png", label: "本地图片" }],
      referenceVideos: ["https://cdn.example/video.mp4"],
    });
    expect(result.productionManifest?.shots[0].referenceLocalImages).toEqual([{ assetId: "image-local", path: "C:/private.png", label: "本地图片" }]);
  });
});
