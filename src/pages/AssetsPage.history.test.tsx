// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useLibraryStore, type LibAsset, type TaskRecord } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { useRunStore } from "../store/useRunStore";

const mocks = vi.hoisted(() => ({
  confirmAction: vi.fn(),
  generateImage: vi.fn(),
  generateVideo: vi.fn(),
  markTaskRetried: vi.fn(),
}));
vi.mock("../lib/novel/api", () => ({ novelWorkList: vi.fn().mockResolvedValue([]) }));
vi.mock("../lib/comic/markdownApi", () => ({ comicMdCatalogList: vi.fn().mockResolvedValue([]) }));
vi.mock("../components/novel/NovelAssetManager", () => ({ default: () => null }));
vi.mock("../lib/dbWrite", () => ({ refreshLibraryHistory: vi.fn().mockResolvedValue({ assets: [], tasks: [] }), markTaskRetried: mocks.markTaskRetried }));
vi.mock("../lib/desktop", () => ({ fileSrc: (path: string) => path, openDialog: vi.fn(), revealItemInDir: vi.fn() }));
vi.mock("../lib/logger", () => ({ logEvent: vi.fn(), loggedInvoke: vi.fn() }));
vi.mock("../lib/confirm", () => ({ confirmAction: mocks.confirmAction }));
vi.mock("../lib/generate", () => ({ generateImage: mocks.generateImage }));
vi.mock("../lib/generateVideo", () => ({ generateVideo: mocks.generateVideo }));

import AssetsPage from "./AssetsPage";

const imageParams = { prompt: "雨夜街道", referencePath: "", size: "1024x1024", quality: "high", background: "opaque", model: "gpt-image-2" };
const failedImage = (patch: Partial<TaskRecord> = {}): TaskRecord => ({
  id: "task-old", nodeId: "gen_txt2img", label: "文生图", model: "gpt-image-2", projectId: "project-a", kind: "image",
  status: "error", createdAt: 1, finishedAt: 2, error: "网关超时", params: imageParams, ...patch,
});
const retryButton = () => screen.queryByRole("button", { name: /按原参数重试/ });

describe("AssetsPage task retry", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.markTaskRetried.mockResolvedValue(undefined);
    useRunStore.setState({ retryingTaskId: null });
    useProjectStore.setState({ activeId: "project-a", projects: [] });
    useLibraryStore.setState({ assets: [], tasks: [failedImage()] });
  });
  afterEach(() => cleanup());

  it("asks before a paid retry and sends nothing when the user cancels", async () => {
    mocks.confirmAction.mockResolvedValue(false);
    render(<AssetsPage onQueueImport={vi.fn()} />);

    fireEvent.click(retryButton()!);

    await waitFor(() => expect(mocks.confirmAction).toHaveBeenCalledWith("按原参数重新生成，将再次计费，继续？"));
    await act(async () => {});
    expect(mocks.generateImage).not.toHaveBeenCalled();
    expect(mocks.markTaskRetried).not.toHaveBeenCalled();
    expect(retryButton()).toBeTruthy();
  });

  it("marks the original task as retried so its button does not come back, even after a reload", async () => {
    mocks.confirmAction.mockResolvedValue(true);
    mocks.generateImage.mockImplementation(async (_params, _provenance, options: { onTaskCreated?: (id: string) => void }) => {
      options.onTaskCreated?.("task-new");
      return [];
    });
    render(<AssetsPage onQueueImport={vi.fn()} />);

    fireEvent.click(retryButton()!);

    expect(await screen.findByText("已重试")).toBeTruthy();
    expect(screen.getByText("重试成功，新结果已保存")).toBeTruthy();
    expect(retryButton()).toBeNull();
    expect(mocks.generateImage).toHaveBeenCalledWith(imageParams, {}, expect.objectContaining({ projectId: "project-a" }));
    expect(mocks.markTaskRetried).toHaveBeenCalledWith("task-old", "task-new");
    expect(useLibraryStore.getState().tasks[0].params?.retriedBy).toBe("task-new");

    // A fresh page over tasks reloaded from the database (where the marker lives).
    cleanup();
    useLibraryStore.getState().loadTasks([failedImage({ params: { ...imageParams, retriedBy: "task-new" } })]);
    render(<AssetsPage onQueueImport={vi.fn()} />);
    expect(screen.getByText("已重试")).toBeTruthy();
    expect(retryButton()).toBeNull();
  });

  it("disables the button while the retry runs and shows a failure on that row", async () => {
    mocks.confirmAction.mockResolvedValue(true);
    let fail!: (error: Error) => void;
    mocks.generateImage.mockReturnValue(new Promise((_resolve, reject) => { fail = reject; }));
    render(<AssetsPage onQueueImport={vi.fn()} />);

    fireEvent.click(retryButton()!);
    await screen.findByText("重试中…");
    expect((retryButton() as HTMLButtonElement).disabled).toBe(true);

    await act(async () => fail(new Error("下载图片失败")));
    expect((await screen.findByRole("alert")).textContent).toBe("重试失败：下载图片失败");
    expect((retryButton() as HTMLButtonElement).disabled).toBe(false);
    expect(mocks.markTaskRetried).not.toHaveBeenCalled();
  });

  it("keeps an unassigned task's retry out of the open project", async () => {
    mocks.confirmAction.mockResolvedValue(true);
    mocks.generateImage.mockResolvedValue([]);
    useLibraryStore.setState({ tasks: [failedImage({ projectId: undefined })] });
    render(<AssetsPage onQueueImport={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "未归属资源" }));
    fireEvent.click(retryButton()!);

    await waitFor(() => expect(mocks.generateImage).toHaveBeenCalledWith(imageParams, {}, expect.objectContaining({ projectId: null })));
  });

  it("confirms and marks a resumed video retry with the new task of the same shot group", async () => {
    mocks.confirmAction.mockResolvedValue(true);
    const videoParams = { shots: [], shotGroupId: "group-1" };
    useLibraryStore.setState({ tasks: [failedImage({ id: "video-old", nodeId: "gen_video_shots", label: "视频镜头 × 2", kind: "video", params: videoParams })] });
    mocks.generateVideo.mockImplementation(async () => {
      useLibraryStore.getState().addTask({ id: "video-new", nodeId: "gen_video_shots", label: "视频镜头 × 2", projectId: "project-a", kind: "video", status: "success", createdAt: 3, params: videoParams });
      return [];
    });
    render(<AssetsPage onQueueImport={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "按原参数重试：视频镜头 × 2" }));

    await waitFor(() => expect(mocks.markTaskRetried).toHaveBeenCalledWith("video-old", "video-new"));
    expect(mocks.confirmAction).toHaveBeenCalledTimes(1);
    expect(mocks.generateVideo).toHaveBeenCalledWith(videoParams, {}, { resumeShotGroupId: "group-1", projectId: "project-a" });
  });

  it("keeps an unassigned video task's retry out of the open project", async () => {
    mocks.confirmAction.mockResolvedValue(true);
    mocks.generateVideo.mockResolvedValue([]);
    useLibraryStore.setState({ tasks: [failedImage({ id: "video-loose", kind: "video", label: "视频镜头 × 1", projectId: undefined, params: { shots: [] } })] });
    render(<AssetsPage onQueueImport={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "未归属资源" }));
    fireEvent.click(retryButton()!);

    await waitFor(() => expect(mocks.generateVideo).toHaveBeenCalledWith({ shots: [] }, {}, expect.objectContaining({ projectId: null })));
  });
});

describe("AssetsPage grid paging", () => {
  const videos = (count: number): LibAsset[] => Array.from({ length: count }, (_, index) => ({
    asset: { id: `video-${index}`, kind: "video", path: `C:/v${index}.mp4` },
    source: `视频 ${index}`,
    projectId: "project-a",
    createdAt: 1_000 - index,
  }));

  beforeEach(() => {
    vi.clearAllMocks();
    useProjectStore.setState({ activeId: "project-a", projects: [] });
    useLibraryStore.setState({ assets: videos(130), tasks: [] });
  });
  afterEach(() => cleanup());

  it("renders 60 cards per grid and reveals 60 more per click until a filter changes", async () => {
    const { container } = render(<AssetsPage onQueueImport={vi.fn()} />);
    const shown = () => container.querySelectorAll("video").length;

    expect(shown()).toBe(60);
    expect(container.querySelector("video")!.getAttribute("preload")).toBe("metadata");

    fireEvent.click(screen.getByRole("button", { name: "加载更多（剩余 70）" }));
    expect(shown()).toBe(120);

    fireEvent.click(screen.getByRole("button", { name: "加载更多（剩余 10）" }));
    expect(shown()).toBe(130);
    expect(screen.queryByRole("button", { name: /加载更多/ })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "视频" }));
    expect(shown()).toBe(60);
    expect(screen.getByRole("button", { name: "加载更多（剩余 70）" })).toBeTruthy();
  });
});
