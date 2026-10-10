// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { LibAsset } from "../store/useLibraryStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { useRunStore } from "../store/useRunStore";
import { useVideoStore } from "../store/useVideoStore";
import type { VideoRunProgress } from "../lib/generateVideo";

const desktop = vi.hoisted(() => ({ openDialog: vi.fn() }));
vi.mock("../lib/desktop", () => ({ fileSrc: (path: string) => path, revealItemInDir: vi.fn(), openDialog: desktop.openDialog }));
const videoApi = vi.hoisted(() => ({ concatVideoAssets: vi.fn(), generateVideo: vi.fn() }));
vi.mock("../lib/generateVideo", () => videoApi);
const confirm = vi.hoisted(() => ({ confirmAction: vi.fn() }));
vi.mock("../lib/confirm", () => confirm);
const externalImport = vi.hoisted(() => ({ importExternalAssets: vi.fn() }));
vi.mock("../lib/externalAssetImport", () => externalImport);
type PickerInput = { action: "prompt" | "merge_prompt" | "reference"; referenceMode: "replace" | "append"; localImageDelivery?: "direct" | "hosting"; entries: unknown[] };
const picker = vi.hoisted(() => ({ onApply: null as ((input: PickerInput) => Promise<void>) | null }));
vi.mock("../components/AssetImportPicker", () => ({
  default: ({ onApply }: { onApply: (input: PickerInput) => Promise<void> }) => {
    picker.onApply = onApply;
    return null;
  },
}));
vi.mock("../lib/comic/markdownApi", () => ({ comicMdCatalogList: vi.fn().mockResolvedValue([]) }));
vi.mock("../lib/ipc", () => ({
  listVideoModels: vi.fn().mockResolvedValue(["model"]),
  fetchModels: vi.fn(),
  listVideoModelCapabilities: vi.fn().mockResolvedValue([{
    id: "model", label: "Model", modes: ["text", "first_frame", "reference"], minDurationS: 1, maxDurationS: 5,
    durationOptions: [1, 2, 3, 4, 5], resolutions: ["720p"], aspectRatios: ["16:9"], maxImages: 4, maxVideos: 1, maxAudios: 0, maxReferenceDurationS: 3, note: "",
  }]),
  mediaHostingGet: vi.fn().mockResolvedValue({ endpoint: "https://host.example/upload", fileField: "file", urlField: "url", authMode: "bearer", hasToken: true, configured: true }),
  assetPublishMedia: vi.fn(),
}));

import VideoPanel from "./VideoPanel";

type RunOptions = { resumeShotGroupId?: string; onProgress?: (progress: VideoRunProgress) => void };

afterEach(() => {
  cleanup();
  picker.onApply = null;
  videoApi.generateVideo.mockReset();
  confirm.confirmAction.mockReset();
  desktop.openDialog.mockReset();
  externalImport.importExternalAssets.mockReset();
  useRunStore.setState({ videoBusy: false, videoError: null, videoPhase: "idle", videoProgress: null, videoResume: null });
});

function project(id: string) {
  return { id, name: "测试项目", description: "", storyStyle: "", artStyle: "", aspectRatio: "16:9", imageModel: "image-model", imageQuality: "high", videoModel: "model", videoResolution: "720p" };
}

function asset(id: string, kind: "image" | "video", path: string): LibAsset {
  return { asset: { id, kind, path }, source: id, projectId: "project-a", createdAt: 1 };
}

function loadShots(count: number) {
  useProjectStore.setState({ activeId: "project-a", projects: [project("project-a"), project("project-b")] });
  useLibraryStore.setState({ assets: [], tasks: [] });
  useVideoStore.getState().load({
    model: "model", mode: "text", aspectRatio: "16:9", resolution: "720p", images: [], videos: [], audios: [],
    shots: Array.from({ length: count }, (_, index) => ({ id: `shot-${index + 1}`, shotNo: index + 1, prompt: `镜头${index + 1}`, durationS: 3 })),
  });
  videoApi.generateVideo.mockResolvedValue([]);
  confirm.confirmAction.mockResolvedValue(true);
}

async function enabledButton(name: string | RegExp) {
  await waitFor(() => expect((screen.getByRole("button", { name }) as HTMLButtonElement).disabled).toBe(false));
  return screen.getByRole("button", { name });
}

/** A 3-shot run that saves shot 1 under group-1, then fails on shot 2. */
function failAfterFirstShot() {
  videoApi.generateVideo.mockImplementationOnce(async (_params: unknown, _provenance: unknown, options: RunOptions) => {
    options.onProgress?.({ shotGroupId: "group-1", total: 3, doneShotNos: [], currentShotNo: 1 });
    useLibraryStore.getState().addAssets([{ id: "saved-1", kind: "video", path: "C:/saved-1.mp4" }], "视频镜头 1/3", { projectId: "project-a", params: { shotGroupId: "group-1", shotNo: 1 } });
    options.onProgress?.({ shotGroupId: "group-1", total: 3, doneShotNos: [1], currentShotNo: null });
    options.onProgress?.({ shotGroupId: "group-1", total: 3, doneShotNos: [1], currentShotNo: 2 });
    throw new Error("第 2 镜生成超时");
  });
}

describe("VideoPanel paid runs", () => {
  it("resumes a partly failed run with its shot group so saved shots are not paid for again", async () => {
    loadShots(3);
    failAfterFirstShot();
    render(<VideoPanel />);
    fireEvent.click(await enabledButton("生成 3 个视频镜头"));
    await waitFor(() => expect(confirm.confirmAction).toHaveBeenCalledWith("将生成 3 个镜头（共 9 秒），每个镜头单独计费，继续？"));

    const resume = await screen.findByRole("button", { name: "继续生成剩余 2 镜" });
    expect(screen.getByRole("button", { name: "重新生成全部 3 镜（已完成 1 镜将再次计费）" })).toBeTruthy();
    // Shown without the "Error: " prefix String() used to add.
    expect(screen.getByText("第 2 镜生成超时")).toBeTruthy();
    expect(videoApi.generateVideo.mock.calls[0][2]).toMatchObject({ resumeShotGroupId: undefined });

    fireEvent.click(resume);
    await waitFor(() => expect(videoApi.generateVideo).toHaveBeenCalledTimes(2));
    // Only the two missing shots are announced as billed, and the group is resumed.
    expect(confirm.confirmAction).toHaveBeenLastCalledWith("将生成 2 个镜头（共 6 秒），每个镜头单独计费，继续？");
    expect(videoApi.generateVideo.mock.calls[1][2]).toMatchObject({ resumeShotGroupId: "group-1" });
    expect(await screen.findByRole("button", { name: "生成 3 个视频镜头" })).toBeTruthy();
    expect(useRunStore.getState().videoResume).toBeNull();
  });

  it("keeps the resume offer across a remount and a trip to another project", async () => {
    loadShots(3);
    failAfterFirstShot();
    const first = render(<VideoPanel />);
    fireEvent.click(await enabledButton("生成 3 个视频镜头"));
    await screen.findByRole("button", { name: "继续生成剩余 2 镜" });
    first.unmount();

    render(<VideoPanel />);
    expect(await screen.findByRole("button", { name: "继续生成剩余 2 镜" })).toBeTruthy();
    // A project switch clears finished results, but the group belongs to
    // project-a and is offered again when the user comes back to it.
    act(() => { useRunStore.getState().clearFinished(); useProjectStore.setState({ activeId: "project-b" }); });
    await waitFor(() => expect(screen.queryByRole("button", { name: /继续生成剩余/ })).toBeNull());
    act(() => useProjectStore.setState({ activeId: "project-a" }));
    expect(await screen.findByRole("button", { name: "继续生成剩余 2 镜" })).toBeTruthy();
  });

  it("warns that a full rerun bills finished shots again and sends nothing when cancelled", async () => {
    loadShots(3);
    failAfterFirstShot();
    render(<VideoPanel />);
    fireEvent.click(await enabledButton("生成 3 个视频镜头"));
    const rerun = await screen.findByRole("button", { name: "重新生成全部 3 镜（已完成 1 镜将再次计费）" });

    confirm.confirmAction.mockResolvedValueOnce(false);
    fireEvent.click(rerun);
    await waitFor(() => expect(confirm.confirmAction).toHaveBeenLastCalledWith("将生成 3 个镜头（共 9 秒），每个镜头单独计费，其中已完成的 1 镜将再次计费；继续？"));
    await act(async () => {});
    expect(videoApi.generateVideo).toHaveBeenCalledTimes(1);

    fireEvent.click(rerun);
    await waitFor(() => expect(videoApi.generateVideo).toHaveBeenCalledTimes(2));
    expect(videoApi.generateVideo.mock.calls[1][2]).toMatchObject({ resumeShotGroupId: undefined });
  });

  it("drops the resume offer as soon as the form changes", async () => {
    loadShots(3);
    failAfterFirstShot();
    render(<VideoPanel />);
    fireEvent.click(await enabledButton("生成 3 个视频镜头"));
    await screen.findByRole("button", { name: "继续生成剩余 2 镜" });

    fireEvent.change(screen.getByLabelText("第 3 镜时长"), { target: { value: "2" } });
    await waitFor(() => expect(screen.queryByRole("button", { name: /继续生成剩余/ })).toBeNull());
    expect(useRunStore.getState().videoResume).toBeNull();
    // Changing it back is still a request that was edited after the failure.
    fireEvent.change(screen.getByLabelText("第 3 镜时长"), { target: { value: "3" } });
    expect(screen.queryByRole("button", { name: /继续生成剩余/ })).toBeNull();

    fireEvent.click(await enabledButton("生成 3 个视频镜头"));
    await waitFor(() => expect(videoApi.generateVideo).toHaveBeenCalledTimes(2));
    expect(videoApi.generateVideo.mock.calls[1][2]).toMatchObject({ resumeShotGroupId: undefined });
  });

  it("asks before a multi-shot run, sends nothing when cancelled, and keeps a single shot one click", async () => {
    loadShots(2);
    confirm.confirmAction.mockResolvedValueOnce(false);
    render(<VideoPanel />);
    fireEvent.click(await enabledButton("生成 2 个视频镜头"));
    await waitFor(() => expect(confirm.confirmAction).toHaveBeenCalledWith("将生成 2 个镜头（共 6 秒），每个镜头单独计费，继续？"));
    await act(async () => {});
    expect(videoApi.generateVideo).not.toHaveBeenCalled();
    expect(useRunStore.getState().videoBusy).toBe(false);

    fireEvent.click(screen.getAllByTitle("删除镜头")[1]);
    fireEvent.click(await enabledButton("生成 1 个视频镜头"));
    await waitFor(() => expect(videoApi.generateVideo).toHaveBeenCalledTimes(1));
    expect(confirm.confirmAction).toHaveBeenCalledTimes(1);
  });

  it("shows which shot is running with an elapsed timer and lights download/save as shots are saved", async () => {
    loadShots(3);
    let report: ((progress: VideoRunProgress) => void) | undefined;
    let finish: ((assets: unknown[]) => void) | undefined;
    videoApi.generateVideo.mockImplementationOnce((_params: unknown, _provenance: unknown, options: RunOptions) => new Promise((resolve) => {
      report = options.onProgress;
      finish = resolve;
    }));
    render(<VideoPanel />);
    fireEvent.click(await enabledButton("生成 3 个视频镜头"));
    await waitFor(() => expect(report).toBeDefined());
    const step = (label: RegExp) => screen.getByText(label).getAttribute("data-state");

    act(() => report?.({ shotGroupId: "g", total: 3, doneShotNos: [], currentShotNo: 1 }));
    expect(screen.getByText(/^第 1\/3 镜 · 已用时 00:0\d$/)).toBeTruthy();
    expect(step(/^逐镜生成 1\/3$/)).toBe("active");
    expect(step(/^分别保存 0\/3$/)).toBe("pending");

    act(() => report?.({ shotGroupId: "g", total: 3, doneShotNos: [1], currentShotNo: 2 }));
    expect(screen.getByText(/^第 2\/3 镜 · 已保存 1 镜 · 已用时 00:0\d$/)).toBeTruthy();
    expect(step(/^逐镜下载 1\/3$/)).toBe("partial");
    expect(step(/^分别保存 1\/3$/)).toBe("partial");

    // The elapsed time ticks on its own between shot reports.
    act(() => {
      const progress = useRunStore.getState().videoProgress!;
      useRunStore.getState().patch({ videoProgress: { ...progress, startedAt: Date.now() - 59_500 } });
    });
    await waitFor(() => expect(screen.getByText(/已用时 01:00$/)).toBeTruthy(), { timeout: 2500 });

    await act(async () => finish?.([]));
    expect(await screen.findByText("全部镜头已分别保存")).toBeTruthy();
    expect(step(/^分别保存$/)).toBe("done");
  });

  it("shows the import notice neutrally and clears it on the next import and on project switch", async () => {
    loadShots(1);
    const imported = asset("imported-image", "image", "D:/imported.png");
    desktop.openDialog.mockResolvedValue(["D:/imported.png"]);
    externalImport.importExternalAssets.mockResolvedValue([imported]);
    render(<VideoPanel />);

    fireEvent.click(screen.getByRole("button", { name: /上传到资产库/ }));
    const notice = await screen.findByRole("status");
    expect(notice.textContent).toContain("已将 1 个外部素材保存到资产库");
    expect(notice.className).not.toMatch(/rose|red/);

    await waitFor(() => expect(picker.onApply).not.toBeNull());
    await act(async () => { await picker.onApply?.({ action: "reference", referenceMode: "replace", localImageDelivery: "direct", entries: [{ entryType: "library_asset", asset: imported }] }); });
    expect(screen.queryByRole("status")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /上传到资产库/ }));
    await screen.findByRole("status");
    act(() => useProjectStore.setState({ activeId: "project-b" }));
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
  });

  it("lets the video grid load only metadata until a video is opened", async () => {
    loadShots(1);
    useLibraryStore.setState({ assets: [asset("grid-video", "video", "C:/grid.mp4")], tasks: [] });
    render(<VideoPanel />);
    const thumbnail = await screen.findByTitle("grid-video");
    expect(thumbnail.tagName).toBe("VIDEO");
    expect(thumbnail.getAttribute("preload")).toBe("metadata");
  });
});
