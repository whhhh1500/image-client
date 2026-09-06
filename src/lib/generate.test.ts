import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./ipc", () => ({ runNode: vi.fn() }));
vi.mock("./dbWrite", () => ({ persistAssets: vi.fn(), persistTask: vi.fn() }));
vi.mock("./logger", () => ({ logEvent: vi.fn() }));

import { persistAssets, persistTask } from "./dbWrite";
import { generateImage } from "./generate";
import { runNode } from "./ipc";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";

const params = {
  prompt: "雨夜里的角色",
  referencePath: "",
  size: "1024x1536 (2:3)",
  quality: "high",
  background: "opaque",
  model: "gpt-image-2",
};

const comicGeneration = {
  comicRunId: "run_1",
  generationAttemptId: "attempt_1",
  kind: "page" as const,
  comicProjectId: "comic_1",
  pageNo: 1,
  role: "full_page",
};

describe("generateImage asset persistence", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useProjectStore.setState({ activeId: "project_1" });
    useLibraryStore.setState({ assets: [], tasks: [] });
    vi.mocked(persistTask).mockResolvedValue();
  });

  it("persists Rust-required comic provenance on the first asset write", async () => {
    const asset = { id: "asset_new", kind: "image" as const, path: "C:/new.png" };
    vi.mocked(runNode).mockResolvedValue({ assets: [asset] });

    await expect(generateImage(params, { comicGeneration })).resolves.toEqual([asset]);

    expect(persistAssets).toHaveBeenCalledWith([asset], "文生图", expect.objectContaining({
      projectId: "project_1",
      comicRunId: "run_1",
      generationAttemptId: "attempt_1",
      comicGeneration,
    }));
  });

  it("passes role-aware multi references and records every local parent asset", async () => {
    const references = [
      { asset: { id: "ref_character", kind: "image" as const, path: "C:/character.png" }, source: "角色", createdAt: 1 },
      { asset: { id: "ref_scene", kind: "image" as const, path: "C:/scene.png" }, source: "场景", createdAt: 2 },
    ];
    useLibraryStore.setState({ assets: references });
    const generated = { id: "asset_multi", kind: "image" as const, path: "C:/multi.png" };
    vi.mocked(runNode).mockResolvedValue({ assets: [generated] });

    await generateImage({
      ...params,
      referencePath: "C:/legacy-ignored.png",
      references: [
        { path: "C:/scene.png", role: "scene", weight: 0.7, sortOrder: 20 },
        { path: "C:/character.png", role: "character_identity", sortOrder: 10 },
      ],
    });

    expect(runNode).toHaveBeenCalledWith(expect.objectContaining({
      config: expect.objectContaining({
        referencePath: "C:/legacy-ignored.png",
        references: [
          { path: "C:/scene.png", role: "scene", weight: 0.7, sortOrder: 20 },
          { path: "C:/character.png", role: "character_identity", sortOrder: 10 },
        ],
      }),
    }));
    expect(persistAssets).toHaveBeenCalledWith([generated], "图生图", expect.objectContaining({
      params: expect.objectContaining({
        provenance: expect.objectContaining({
          parentAssetIds: expect.arrayContaining(["ref_character", "ref_scene"]),
          sourceMaterials: expect.arrayContaining([
            expect.objectContaining({ assetId: "ref_character", label: "角色身份参考" }),
            expect.objectContaining({ assetId: "ref_scene", label: "场景参考" }),
          ]),
        }),
      }),
    }));
  });

  it("keeps the legacy single reference contract when references are absent", async () => {
    const reference = { asset: { id: "ref_legacy", kind: "image" as const, path: "C:/legacy.png" }, source: "旧参考", createdAt: 1 };
    useLibraryStore.setState({ assets: [reference] });
    const generated = { id: "asset_legacy", kind: "image" as const, path: "C:/legacy-out.png" };
    vi.mocked(runNode).mockResolvedValue({ assets: [generated] });

    await generateImage({ ...params, referencePath: "C:/legacy.png" });

    const request = vi.mocked(runNode).mock.calls[0][0];
    expect(request.config.referencePath).toBe("C:/legacy.png");
    expect(request.config).not.toHaveProperty("references");
  });

  it("rejects empty, preexisting, and duplicate provider asset IDs before any asset write", async () => {
    const existing = { asset: { id: "asset_old", kind: "image" as const, path: "C:/old.png" }, source: "历史", createdAt: 1 };
    useLibraryStore.setState({ assets: [existing] });
    vi.mocked(runNode).mockResolvedValue({ assets: [
      { id: "asset_old", kind: "image", path: "C:/overwritten.png" },
      { id: "asset_old", kind: "image", path: "C:/duplicate.png" },
      { id: "", kind: "image", path: "C:/empty.png" },
    ] });

    await expect(generateImage(params)).rejects.toThrow("无效或重复的资产 ID");

    expect(persistAssets).not.toHaveBeenCalled();
    expect(useLibraryStore.getState().assets).toEqual([existing]);
  });

  it("treats an empty provider asset list as an error without writing assets", async () => {
    vi.mocked(runNode).mockResolvedValue({ assets: [] });

    await expect(generateImage(params)).rejects.toThrow("没有返回图像产物");

    expect(persistAssets).not.toHaveBeenCalled();
    expect(persistTask).toHaveBeenLastCalledWith(expect.objectContaining({ status: "error" }));
    expect(useLibraryStore.getState().tasks[0]).toMatchObject({ status: "error" });
  });

  it("returns a persisted asset when the trailing success task-history write fails", async () => {
    const asset = { id: "asset_new", kind: "image" as const, path: "C:/new.png" };
    vi.mocked(runNode).mockResolvedValue({ assets: [asset] });
    vi.mocked(persistTask).mockResolvedValueOnce().mockRejectedValueOnce(new Error("task history unavailable"));

    await expect(generateImage(params, { comicGeneration })).resolves.toEqual([asset]);

    expect(persistAssets).toHaveBeenCalledOnce();
    expect(useLibraryStore.getState().assets[0]).toMatchObject({ asset, projectId: "project_1" });
  });

  it("still fails when the first asset persistence write fails", async () => {
    vi.mocked(runNode).mockResolvedValue({ assets: [{ id: "asset_new", kind: "image", path: "C:/new.png" }] });
    vi.mocked(persistAssets).mockRejectedValueOnce(new Error("asset database unavailable"));

    await expect(generateImage(params)).rejects.toThrow("asset database unavailable");
    expect(useLibraryStore.getState().assets).toEqual([]);
  });
});
