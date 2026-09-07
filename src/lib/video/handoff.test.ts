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
});
