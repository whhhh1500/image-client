import { beforeEach, describe, expect, it, vi } from "vitest";

const assetImportFiles = vi.hoisted(() => vi.fn());
vi.mock("./ipc", () => ({ assetImportFiles }));

import { useLibraryStore } from "../store/useLibraryStore";
import { importExternalAssets } from "./externalAssetImport";

describe("importExternalAssets", () => {
  beforeEach(() => {
    assetImportFiles.mockReset();
    useLibraryStore.setState({ assets: [], tasks: [] });
  });

  it("registers only atomically persisted native results and preserves their metadata", async () => {
    assetImportFiles.mockResolvedValue([{
      asset: { id: "uploaded", kind: "image", path: "D:/assets/uploaded.png", format: "png" },
      source: "original.png",
      projectId: "project-a",
      params: { origin: "external_upload", catalog: { version: 1, category: "upload", origin: "local_upload", group: { type: "upload_batch", id: "batch-a" } } },
      createdAt: 42,
    }]);
    const result = await importExternalAssets({
      projectId: "project-a",
      importEntry: "image_reference",
      uploadBatchId: "batch-a",
      files: [{ path: "D:/picked/original.png" }],
      params: { callerScope: "image" },
    });
    expect(assetImportFiles).toHaveBeenCalledWith(expect.objectContaining({
      projectId: "project-a", importEntry: "image_reference", uploadBatchId: "batch-a",
      files: [{ source: "path", path: "D:/picked/original.png" }],
      params: { callerScope: "image" },
    }));
    expect(result[0].createdAt).toBe(42);
    expect(useLibraryStore.getState().assets).toEqual(result);
  });

  it("does not mutate the library when native atomic import fails", async () => {
    assetImportFiles.mockRejectedValue(new Error("登记外部资产失败"));
    await expect(importExternalAssets({ projectId: "project-a", importEntry: "image_reference", files: [{ path: "D:/picked/bad.png" }] }))
      .rejects.toThrow("登记外部资产失败");
    expect(useLibraryStore.getState().assets).toEqual([]);
  });

  it("rejects an oversized browser multi-select before reading File bytes", async () => {
    const file = { name: "large.mp4", size: 513 * 1024 * 1024, arrayBuffer: vi.fn() } as unknown as File;
    await expect(importExternalAssets({ projectId: "project-a", importEntry: "video_reference", files: [{ file }] }))
      .rejects.toThrow("导入批次超过 512 MiB 大小限制");
    expect(file.arrayBuffer).not.toHaveBeenCalled();
    expect(assetImportFiles).not.toHaveBeenCalled();
  });
});
