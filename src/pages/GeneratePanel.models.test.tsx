// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { fetchModels, runNode } from "../lib/ipc";
import { persistAssets, persistTask } from "../lib/dbWrite";
import { useGenerationStore } from "../store/useGenerationStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { useRunStore } from "../store/useRunStore";
import GeneratePanel from "./GeneratePanel";

vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (path: string) => path }));
const progressListeners = vi.hoisted(() => new Set<(event: { payload: unknown }) => void>());
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, handler: (event: { payload: unknown }) => void) => {
    progressListeners.add(handler);
    return () => progressListeners.delete(handler);
  }),
}));
/** Delivers a backend progress step to the run that `runNode` was tagged with. */
function emitProgress(payload: Record<string, unknown>) {
  const calls = vi.mocked(runNode).mock.calls;
  const progressId = calls[calls.length - 1]?.[1];
  for (const handler of progressListeners) handler({ payload: { progressId, ...payload } });
}
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
  useRunStore.setState({ imageBusy: false, imageError: null, imageBatchIds: [], imageProgress: null, imageRunStartedAt: null, imageLog: [] });
  progressListeners.clear();
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
      }), expect.stringMatching(/^task/));
    });
    // The batch is rendered together in the preview instead of one newest card.
    expect(await screen.findByText("本次生成 3 张 · 点图放大")).toBeTruthy();
    expect(persistAssets).toHaveBeenCalledWith(batch, "文生图", expect.objectContaining({ params: expect.objectContaining({ count: 3 }) }));
    for (const asset of batch) {
      await waitFor(() => expect(useLibraryStore.getState().assets.some((item) => item.asset.id === asset.id)).toBe(true));
    }
  });

  it("keeps the button locked after a remount while the paid request is still running", async () => {
    let finish: (value: { assets: { id: string; kind: "image"; path: string }[] }) => void = () => {};
    vi.mocked(runNode).mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    useGenerationStore.getState().set({ prompt: "一只猫", count: 1 });
    render(<GeneratePanel />);
    fireEvent.click(screen.getByRole("button", { name: "生成" }));
    await waitFor(() => expect(runNode).toHaveBeenCalledTimes(1));

    // Switching tabs unmounts the panel; coming back must not re-enable it.
    cleanup();
    render(<GeneratePanel />);
    const locked = screen.getByRole("button", { name: /^生成中…/ }) as HTMLButtonElement;
    expect(locked.disabled).toBe(true);
    fireEvent.click(locked);
    expect(runNode).toHaveBeenCalledTimes(1);

    finish({ assets: [{ id: "asset_late", kind: "image", path: "C:/late.png" }] });
    await waitFor(() => expect((screen.getByRole("button", { name: "生成" }) as HTMLButtonElement).disabled).toBe(false));
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
    }), expect.stringMatching(/^task/)));
    await waitFor(() => expect(useLibraryStore.getState().assets).toHaveLength(1));
    expect(screen.queryByText(/本次生成 \d+ 张/)).toBeNull();
  });
});

describe("GeneratePanel run progress", () => {
  it("shows backend steps on the button and keeps them in the run log", async () => {
    let finish: (value: { assets: { id: string; kind: "image"; path: string }[] }) => void = () => {};
    vi.mocked(runNode).mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    useGenerationStore.getState().set({ prompt: "一只猫", count: 1 });
    render(<GeneratePanel />);
    fireEvent.click(screen.getByRole("button", { name: "生成" }));
    await waitFor(() => expect(runNode).toHaveBeenCalledTimes(1));

    act(() => emitProgress({ stage: "waiting", message: "提交文生图请求，等待模型生成" }));
    expect(screen.getByRole("button", { name: /^等待模型生成… \d+s$/ })).toBeTruthy();

    act(() => emitProgress({ stage: "downloading", message: "下载第 1/1 张图片", receivedBytes: 1024, totalBytes: 4096, index: 1, count: 1 }));
    expect(screen.getByRole("button", { name: "下载图片 25%" })).toBeTruthy();

    act(() => emitProgress({ stage: "retrying", message: "第 1/1 张下载中断：超时；1 秒后续传（第 2/4 次）", receivedBytes: 2048, index: 1, count: 1 }));
    act(() => emitProgress({ stage: "downloading", message: "下载第 1/1 张图片", receivedBytes: 3072, totalBytes: 4096, index: 1, count: 1 }));
    expect(screen.getByRole("button", { name: "下载图片 75%" })).toBeTruthy();

    finish({ assets: [{ id: "asset_logged", kind: "image", path: "C:/logged.png" }] });
    await waitFor(() => expect((screen.getByRole("button", { name: "生成" }) as HTMLButtonElement).disabled).toBe(false));

    const lines = [...screen.getByRole("list", { name: "生成日志" }).querySelectorAll("li")].map((item) => item.textContent ?? "");
    expect(lines).toHaveLength(6);
    expect(lines[0]).toMatch(/开始文生图：gpt-image-2 · 1024x1024 · 1 张$/);
    expect(lines[1]).toMatch(/提交文生图请求，等待模型生成$/);
    expect(lines[2]).toMatch(/下载第 1\/1 张图片$/);
    expect(lines[3]).toMatch(/续传（第 2\/4 次）（已收到 2 KB）$/);
    expect(lines[4]).toMatch(/下载第 1\/1 张图片$/);
    expect(lines[5]).toMatch(/完成，已保存 1 张$/);
    expect(useRunStore.getState().imageProgress).toBeNull();
  });

  it("logs the failure reason once the run ends", async () => {
    vi.mocked(runNode).mockRejectedValue("下载图片失败（已尝试 4 次）: 503 Service Unavailable");
    useGenerationStore.getState().set({ prompt: "一只猫", count: 1 });
    render(<GeneratePanel />);
    fireEvent.click(screen.getByRole("button", { name: "生成" }));

    await waitFor(() => expect(screen.getByRole("list", { name: "生成日志" }).textContent).toContain("失败：下载图片失败（已尝试 4 次）"));
  });
});
