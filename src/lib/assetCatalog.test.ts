import { describe, expect, it } from "vitest";
import { assetMatchesCatalogCategory, catalogMetadata, classifyAssetCatalog, groupCatalogAssets } from "./assetCatalog";
import type { LibAsset } from "../store/useLibraryStore";

function asset(id: string, kind: "text" | "image" | "video", params: Record<string, unknown> = {}): LibAsset {
  return { asset: { id, kind, path: `C:/${id}` }, source: "不参与分类", projectId: "project-a", params, createdAt: Number(id.replace(/\D/g, "")) || 1 };
}

describe("asset catalog", () => {
  it("uses explicit catalog metadata before legacy domain fallbacks", () => {
    const item = asset("upload-1", "image", catalogMetadata("upload", "local_upload", { type: "upload_batch", id: "batch-a" }));
    expect(classifyAssetCatalog(item)).toMatchObject({ category: "upload", origin: "local_upload", group: { type: "upload_batch", id: "batch-a" }, isLegacy: false });
    const comicStyle = asset("style-upload-new", "image", { comicStyleReference: true, novelWorkId: "work", ...catalogMetadata("upload", "local_upload", { type: "comic_work", id: "work" }) });
    expect(classifyAssetCatalog(comicStyle)).toMatchObject({ category: "upload", origin: "local_upload", group: { type: "comic_work", id: "work" }, isLegacy: false });
  });

  it("prioritizes reliable product ownership over inherited novel lineage", () => {
    expect(classifyAssetCatalog(asset("novel", "text", { sourceKind: "novel_chapter", novelWorkId: "work", novelChapterId: "chapter", novelChapterRevisionId: "revision", documentType: "novel", agentId: "novel_source" }))).toMatchObject({ category: "novel", origin: "system_mirror", group: { type: "novel_chapter", id: "work::chapter" } });
    expect(classifyAssetCatalog(asset("short", "text", { videoWorkflowId: "workflow" }))).toMatchObject({ category: "short_drama", group: { type: "short_drama_workflow", id: "workflow" } });
    expect(classifyAssetCatalog(asset("short-from-novel", "text", { videoWorkflowId: "workflow", sourceKind: "novel_chapter", novelWorkId: "work", novelChapterId: "chapter", novelChapterRevisionId: "revision" }))).toMatchObject({ category: "short_drama", group: { type: "short_drama_workflow", id: "workflow" } });
    expect(classifyAssetCatalog(asset("style-upload", "image", { comicStyleReference: true, novelWorkId: "work" }))).toMatchObject({ category: "upload", origin: "local_upload" });
    expect(classifyAssetCatalog(asset("video-upload", "video", { videoWorkflowId: "workflow", videoWorkReference: true }))).toMatchObject({ category: "upload", origin: "local_upload" });
    expect(classifyAssetCatalog({ ...asset("words", "image"), source: "漫画生成图片" })).toMatchObject({ category: "legacy", isLegacy: true });
  });

  it("groups generated video shots while retaining isolated legacy assets", () => {
    const grouped = groupCatalogAssets([
      asset("shot1", "video", { shotGroupId: "shots" }),
      asset("shot2", "video", { shotGroupId: "shots" }),
      asset("old", "image"),
    ]);
    expect(grouped).toHaveLength(2);
    expect(grouped.find((item) => item.group?.id === "shots")?.assets).toHaveLength(2);
    expect(grouped.find((item) => !item.group)?.assets[0].classification.category).toBe("legacy");
    expect(assetMatchesCatalogCategory(asset("old", "image"), "upload")).toBe(false);
  });

  it("does not combine matching group ids from different projects", () => {
    const first = asset("p1-shot", "video", { shotGroupId: "shared" });
    const second = { ...asset("p2-shot", "video", { shotGroupId: "shared" }), projectId: "project-b" };
    expect(groupCatalogAssets([first, second])).toHaveLength(2);
  });
});
