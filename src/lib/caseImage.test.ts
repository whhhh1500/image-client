import { beforeEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({ cachePromptlibImage: vi.fn(), listCachedPromptlibImages: vi.fn() }));
const importExternalAssets = vi.hoisted(() => vi.fn());
const activeId = vi.hoisted(() => ({ value: "project-a" as string | null }));
vi.mock("./ipc", () => ipc);
vi.mock("./externalAssetImport", () => ({ importExternalAssets }));
vi.mock("./logger", () => ({ logEvent: vi.fn() }));
vi.mock("../store/useProjectStore", () => ({ useProjectStore: { getState: () => ({ activeId: activeId.value }) } }));

import { hydrateCaseImageCache, importCaseImage } from "./caseImage";

describe("Prompt Library case image import", () => {
  beforeEach(async () => {
    ipc.cachePromptlibImage.mockReset();
    ipc.listCachedPromptlibImages.mockReset();
    importExternalAssets.mockReset();
    activeId.value = "project-a";
    ipc.listCachedPromptlibImages.mockResolvedValue(["D:/cache/case999.png"]);
    await hydrateCaseImageCache();
  });

  it("uses the atomic project-scoped upload helper and returns its library asset", async () => {
    const asset = { asset: { id: "case-asset", kind: "image", path: "D:/assets/case.png" }, source: "case999.png", projectId: "project-a", params: {}, createdAt: 10 };
    importExternalAssets.mockResolvedValue([asset]);
    await expect(importCaseImage({ id: "case:999", image: "case999.png" } as never)).resolves.toEqual(asset);
    expect(importExternalAssets).toHaveBeenCalledWith(expect.objectContaining({
      projectId: "project-a",
      importEntry: "prompt_library_reference",
      files: [{ path: "D:/cache/case999.png" }],
      params: { promptLibraryEntryId: "case:999", promptLibraryCaseImage: true },
    }));
  });
});
