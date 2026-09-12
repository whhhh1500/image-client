// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { fetchModels, runNode } from "../lib/ipc";
import { persistAssets, persistTask } from "../lib/dbWrite";
import { useGenerationStore } from "../store/useGenerationStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import GeneratePanel from "./GeneratePanel";

vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (path: string) => path }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("../lib/ipc", () => ({
  fetchModels: vi.fn(),
  runNode: vi.fn(),
  saveMediaAsset: vi.fn(),
  inspectImageFile: vi.fn().mockResolvedValue({ path: "", directory: "", fileName: "", bytes: 0, kb: 0, displayPath: "", variants: [] }),
}));
vi.mock("../lib/dbWrite", () => ({ persistAssets: vi.fn().mockResolvedValue(undefined), persistTask: vi.fn().mockResolvedValue(undefined) }));
vi.mock("../lib/comic/markdownApi", () => ({ comicMdCatalogList: vi.fn().mockResolvedValue([]) }));

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(persistTask).mockResolvedValue(undefined);
  useGenerationStore.getState().reset();
  useLibraryStore.setState({ assets: [], tasks: [] });
  useProjectStore.setState({
    activeId: "p1",
    projects: [{
      id: "p1", name: "项目", description: "", storyStyle: "", artStyle: "", aspectRatio: "16:9",
      imageModel: "gpt-image-2", imageQuality: "high", videoModel: "kling-video-v3", videoResolution: "720p",
    }],
  });
});

afterEach(() => {
  cleanup();
  vi.mocked(fetchModels).mockReset();
});

describe("GeneratePanel model switcher", () => {
  it("fetches the image catalogue from the saved config and accepts a hand-typed model", async () => {
    vi.mocked(fetchModels).mockResolvedValue(["gpt-image-9", "gpt-image-9-mini"]);
    render(<GeneratePanel />);

    const input = screen.getByRole("combobox", { name: "图像模型" }) as HTMLInputElement;
    expect(input.value).toBe("gpt-image-2");

    fireEvent.click(screen.getByRole("button", { name: "获取模型（图像模型）" }));
    await screen.findByText(/已获取 2 个模型/);
    expect(vi.mocked(fetchModels)).toHaveBeenCalledWith({ url: "", key: "", kind: "image" });

    fireEvent.click(screen.getByRole("button", { name: "展开图像模型列表" }));
    expect(screen.queryByRole("option", { name: "gpt-image-2" })).toBeNull();
    fireEvent.click(screen.getByRole("option", { name: "gpt-image-9-mini" }));
    expect(useGenerationStore.getState().model).toBe("gpt-image-9-mini");

    fireEvent.change(screen.getByRole("combobox", { name: "图像模型" }), {
      target: { value: "自填图像模型" },
    });
    expect(useGenerationStore.getState().model).toBe("自填图像模型");
  });

  it("keeps the current model when the gateway catalogue cannot be read", async () => {
    vi.mocked(fetchModels).mockRejectedValue("请先填写 API Key");
    render(<GeneratePanel />);

    fireEvent.click(screen.getByRole("button", { name: "获取模型（图像模型）" }));

    await screen.findByText("获取模型失败：请先填写 API Key");
    expect(useGenerationStore.getState().model).toBe("gpt-image-2");
  });

  it("explains the Grok parameter contract only for grok models", async () => {
    const { unmount } = render(<GeneratePanel />);
    expect(screen.queryByText(/Grok 按比例\+分辨率提交/)).toBeNull();
    expect(screen.queryByText(/Grok 不支持，提交时忽略/)).toBeNull();
    unmount();

    useGenerationStore.getState().set({ model: "grok-3-image" });
    render(<GeneratePanel />);
    expect(await screen.findByText("Grok 按比例+分辨率提交（最高 2k）")).toBeTruthy();
    expect(await screen.findByText("Grok 不支持，提交时忽略")).toBeTruthy();
  });

  it("generates the requested number of images and shows the whole batch", async () => {
    const batch = [1, 2, 3].map((index) => ({ id: `asset_${index}`, kind: "image" as const, path: `C:/out-${index}.png` }));
    vi.mocked(runNode).mockResolvedValue({ assets: batch });
    useGenerationStore.getState().set({ prompt: "三只猫", count: 3 });
    render(<GeneratePanel />);

    const countSelect = screen.getByRole("combobox", { name: /数量/ }) as HTMLSelectElement;
    expect(countSelect.value).toBe("3");
    expect([...countSelect.options].map((option) => option.value)).toEqual(["1", "2", "3", "4"]);
    expect(screen.getByText(/3 张按张计费/)).toBeTruthy();
    const button = screen.getByRole("button", { name: "生成 3 张" }) as HTMLButtonElement;
    expect(button.disabled).toBe(false);

    fireEvent.click(button);

    await waitFor(() => {
      expect(runNode).toHaveBeenCalledWith(expect.objectContaining({
        config: expect.objectContaining({ n: 3, prompt: "三只猫" }),
      }));
    });
    // The batch is rendered together in the preview instead of one newest card.
    expect(await screen.findByText("本次生成 3 张 · 点图放大")).toBeTruthy();
    expect(persistAssets).toHaveBeenCalledWith(batch, "文生图", expect.objectContaining({ params: expect.objectContaining({ count: 3 }) }));
    for (const asset of batch) {
      await waitFor(() => expect(useLibraryStore.getState().assets.some((item) => item.asset.id === asset.id)).toBe(true));
    }
  });

  it("keeps a single-image run on the classic preview", async () => {
    vi.mocked(runNode).mockResolvedValue({ assets: [{ id: "asset_single", kind: "image", path: "C:/single.png" }] });
    useGenerationStore.getState().set({ prompt: "一只猫", count: 1 });
    render(<GeneratePanel />);

    const countSelect = screen.getByRole("combobox", { name: /数量/ }) as HTMLSelectElement;
    expect(countSelect.value).toBe("1");
    expect(screen.queryByText(/按张计费/)).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "生成" }));

    await waitFor(() => expect(runNode).toHaveBeenCalledWith(expect.objectContaining({
      config: expect.objectContaining({ n: 1 }),
    })));
    await waitFor(() => expect(useLibraryStore.getState().assets).toHaveLength(1));
    expect(screen.queryByText(/本次生成 \d+ 张/)).toBeNull();
  });
});
