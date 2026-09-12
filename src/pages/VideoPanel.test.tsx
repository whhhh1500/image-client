// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { LibAsset } from "../store/useLibraryStore";
import { createEmptyStoryboardShot, serializeStoryboard } from "../lib/video/storyboard";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { productionManifestMismatch, useVideoStore } from "../store/useVideoStore";
import type { VideoModelCapability } from "../lib/ipc";
import { isPublicHttpsUrl } from "../lib/video/referenceUrl";

vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (path: string) => path }));
vi.mock("@tauri-apps/plugin-opener", () => ({ revealItemInDir: vi.fn() }));
const videoApi = vi.hoisted(() => ({ concatVideoAssets: vi.fn(), generateVideo: vi.fn().mockResolvedValue([]) }));
vi.mock("../lib/generateVideo", () => videoApi);
const mediaHosting = vi.hoisted(() => ({
  get: vi.fn().mockResolvedValue({ endpoint: "https://host.example/upload", fileField: "file", urlField: "url", authMode: "bearer", hasToken: true, configured: true }),
  publish: vi.fn(),
}));
type PickerInput = { action: "prompt" | "merge_prompt" | "reference"; referenceMode: "replace" | "append"; localImageDelivery?: "direct" | "hosting"; entries: unknown[] };
const picker = vi.hoisted(() => ({ onApply: null as ((input: PickerInput) => void) | null, kinds: [] as string[], version: 0 }));
vi.mock("../components/AssetImportPicker", () => ({
  default: ({ onApply, kinds }: { onApply: (input: PickerInput) => void; kinds: string[] }) => {
    picker.onApply = onApply; picker.version += 1;
    picker.kinds = kinds;
    return null;
  },
}));
vi.mock("../lib/comic/markdownApi", () => ({ comicMdCatalogList: vi.fn().mockResolvedValue([]) }));
const modelCatalog = vi.hoisted(() => ({ fetch: vi.fn() }));
vi.mock("../lib/ipc", () => ({
  listVideoModels: vi.fn().mockResolvedValue(["model"]),
  fetchModels: modelCatalog.fetch,
  listVideoModelCapabilities: vi.fn().mockResolvedValue([{
    id: "model",
    label: "Model",
    modes: ["text", "first_frame", "reference"],
    minDurationS: 1,
    maxDurationS: 5,
    durationOptions: [1, 2, 3, 4, 5],
    resolutions: ["720p"],
    aspectRatios: ["16:9"],
    maxImages: 4,
    maxVideos: 1,
    maxAudios: 0,
    maxReferenceDurationS: 3,
    note: "",
  }]),
  mediaHostingGet: mediaHosting.get,
  assetPublishMedia: mediaHosting.publish,
}));

import VideoPanel, {
  durationOptionsFor,
  resolveImportedStoryboardShots,
  shotReferenceSummary,
  unresolvedShotReferenceIds,
} from "./VideoPanel";

afterEach(() => {
  picker.onApply = null;
  modelCatalog.fetch.mockReset();
  mediaHosting.get.mockReset().mockResolvedValue({ endpoint: "https://host.example/upload", fileField: "file", urlField: "url", authMode: "bearer", hasToken: true, configured: true });
  mediaHosting.publish.mockReset();
  videoApi.generateVideo.mockReset().mockResolvedValue([]);
});

function asset(id: string, kind: "image" | "video" | "text", projectId: string, path = `https://cdn.example/${id}`): LibAsset {
  return { asset: { id, kind, path }, source: id, projectId, createdAt: 1 };
}

function project(id: string) {
  return { id, name: "测试项目", description: "", storyStyle: "", artStyle: "", aspectRatio: "16:9", imageModel: "image-model", imageQuality: "high", videoModel: "model", videoResolution: "720p" };
}

describe("VideoPanel storyboard import", () => {
  it("offers video assets and imports a public HTTPS video into the first shot provider references", async () => {
    useProjectStore.setState({ activeId: "project-a", projects: [{ id: "project-a", name: "测试项目", description: "", storyStyle: "", artStyle: "", aspectRatio: "16:9", imageModel: "image-model", imageQuality: "high", videoModel: "model", videoResolution: "720p" }] });
    useVideoStore.getState().load({ model: "model", mode: "text", aspectRatio: "16:9", resolution: "720p", images: [], videos: [], audios: [], shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 3 }] });
    const video = asset("video-ref", "video", "project-a", "https://cdn.example/reference.mp4");
    useLibraryStore.setState({ assets: [video], tasks: [] });

    render(<VideoPanel />);
    await waitFor(() => expect(picker.kinds).toContain("video"));
    act(() => picker.onApply?.({ action: "reference", referenceMode: "replace", entries: [{ entryType: "library_asset", asset: video }] }));

    expect(useVideoStore.getState()).toMatchObject({
      mode: "reference",
      shots: [{ referenceStrategy: "reference", referenceAssetIds: ["video-ref"], referenceVideos: ["https://cdn.example/reference.mp4"] }],
    });
  });

  it("keeps a local PNG as a target-shot reference while hosting only the local MP4", async () => {
    useProjectStore.setState({ activeId: "project-a", projects: [{ id: "project-a", name: "测试项目", description: "", storyStyle: "", artStyle: "", aspectRatio: "16:9", imageModel: "image-model", imageQuality: "high", videoModel: "model", videoResolution: "720p" }] });
    useVideoStore.getState().load({ model: "model", mode: "text", aspectRatio: "16:9", resolution: "720p", images: [], videos: [], audios: [], shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 3 }] });
    const image = asset("local-image", "image", "project-a", "D:/local-image.png");
    const video = asset("local-video", "video", "project-a", "D:/local-video.mp4");
    useLibraryStore.setState({ assets: [image, video], tasks: [] });
    mediaHosting.publish.mockResolvedValue({ results: [
      { key: "asset:local-video", url: "https://cdn.example/local-video.mp4", sha256: "video-sha" },
    ] });

    render(<VideoPanel />);
    await waitFor(() => expect(picker.onApply).not.toBeNull());
    await act(async () => { await picker.onApply?.({ action: "reference", referenceMode: "replace", entries: [{ entryType: "library_asset", asset: image }, { entryType: "library_asset", asset: video }] }); });
    expect(mediaHosting.publish).toHaveBeenCalledWith({ projectId: "project-a", expectedEndpoint: "https://host.example/upload", sources: [{ assetId: "local-video" }] });
    expect(useVideoStore.getState().shots[0]).toMatchObject({ referenceLocalImages: [{ assetId: "local-image", path: "D:/local-image.png", label: "local-image" }], referenceVideos: ["https://cdn.example/local-video.mp4"] });
    expect(useVideoStore.getState().importedSources?.[0].sourceMaterials).toEqual(expect.arrayContaining([
      expect.objectContaining({ path: "D:/local-image.png", assetId: "local-image" }),
      expect.objectContaining({ path: "D:/local-video.mp4", publishedUrl: "https://cdn.example/local-video.mp4", sha256: "video-sha" }),
    ]));
    fireEvent.click(screen.getByRole("button", { name: /生成 1 个视频镜头/ }));
    await waitFor(() => expect(videoApi.generateVideo).toHaveBeenCalledWith(expect.objectContaining({ shots: [expect.objectContaining({ referenceLocalImages: [expect.objectContaining({ assetId: "local-image" })], referenceVideos: ["https://cdn.example/local-video.mp4"] })] }), expect.any(Object)));
  });

  it("sends one local image directly as a first frame and submits the enabled generation form", async () => {
    useProjectStore.setState({ activeId: "project-a", projects: [project("project-a")] });
    useVideoStore.getState().load({ model: "model", mode: "text", aspectRatio: "16:9", resolution: "720p", images: [], videos: [], audios: [], shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 3 }] });
    const image = asset("direct-image", "image", "project-a", "D:/direct.png");
    useLibraryStore.setState({ assets: [image], tasks: [] });

    render(<VideoPanel />);
    await waitFor(() => expect(picker.onApply).not.toBeNull());
    await act(async () => { await picker.onApply?.({ action: "reference", referenceMode: "replace", localImageDelivery: "direct", entries: [{ entryType: "library_asset", asset: image }] }); });

    expect(mediaHosting.publish).not.toHaveBeenCalled();
    expect(useVideoStore.getState()).toMatchObject({ mode: "first_frame", shots: [expect.objectContaining({ referenceStrategy: "first_frame", referenceLocalImages: [expect.objectContaining({ assetId: "direct-image", path: "D:/direct.png" })] })] });
    const generate = screen.getByRole("button", { name: /生成 1 个视频镜头/ }) as HTMLButtonElement;
    expect(generate.disabled).toBe(false);
    fireEvent.click(generate);
    await waitFor(() => expect(videoApi.generateVideo).toHaveBeenCalledWith(expect.objectContaining({ shots: [expect.objectContaining({ referenceLocalImages: [expect.objectContaining({ assetId: "direct-image" })] })] }), expect.any(Object)));
  });

  it("keeps canonical comic images as direct local identities", async () => {
    useProjectStore.setState({ activeId: "project-a", projects: [project("project-a")] });
    useVideoStore.getState().load({ model: "model", mode: "text", aspectRatio: "16:9", resolution: "720p", images: [], videos: [], audios: [], shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 3 }] });
    render(<VideoPanel />);
    await waitFor(() => expect(picker.onApply).not.toBeNull());
    await act(async () => { await picker.onApply?.({
      action: "reference", referenceMode: "replace", localImageDelivery: "direct", entries: [{
        entryType: "canonical_comic", readonly: true, sourceUri: "comic://work/page-1", projectId: "project-a", kind: "image", title: "漫画第 1 页", path: "D:/comic-page.png", createdAt: 1,
      }],
    }); });
    expect(mediaHosting.publish).not.toHaveBeenCalled();
    expect(useVideoStore.getState().shots[0].referenceLocalImages).toEqual([{ sourceUri: "comic://work/page-1", path: "D:/comic-page.png", label: "漫画第 1 页" }]);
  });

  it("keeps the remaining delivery path and provenance when the same image is removed from the other path", async () => {
    useProjectStore.setState({ activeId: "project-a", projects: [project("project-a")] });
    useVideoStore.getState().load({ model: "model", mode: "text", aspectRatio: "16:9", resolution: "720p", images: [], videos: [], audios: [], shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 3 }] });
    const image = asset("dual-image", "image", "project-a", "D:/dual.png");
    useLibraryStore.setState({ assets: [image], tasks: [] });
    mediaHosting.publish.mockResolvedValue({ results: [{ key: "asset:dual-image", url: "https://cdn.example/dual.png", sha256: "local-bytes-sha" }] });

    render(<VideoPanel />);
    await waitFor(() => expect(picker.onApply).not.toBeNull());
    const input = { action: "reference" as const, entries: [{ entryType: "library_asset" as const, asset: image }] };
    await act(async () => { await picker.onApply?.({ ...input, referenceMode: "replace", localImageDelivery: "direct" }); });
    await act(async () => { await picker.onApply?.({ ...input, referenceMode: "append", localImageDelivery: "hosting" }); });
    expect(mediaHosting.publish).toHaveBeenCalledWith({ projectId: "project-a", expectedEndpoint: "https://host.example/upload", sources: [{ assetId: "dual-image" }] });
    expect(useVideoStore.getState().shots[0]).toMatchObject({ referenceImages: ["https://cdn.example/dual.png"], referenceLocalImages: [expect.objectContaining({ assetId: "dual-image" })], referenceAssetIds: ["dual-image"] });

    fireEvent.click(screen.getByRole("button", { name: "移除托管图片 https://cdn.example/dual.png" }));
    expect(useVideoStore.getState().shots[0]).toMatchObject({ referenceImages: [], referenceLocalImages: [expect.objectContaining({ assetId: "dual-image" })], referenceAssetIds: ["dual-image"] });
    expect(useVideoStore.getState().importedSources?.flatMap((record) => record.sourceMaterials)).toEqual(expect.arrayContaining([expect.objectContaining({ assetId: "dual-image", path: "D:/dual.png" })]));

    await act(async () => { await picker.onApply?.({ ...input, referenceMode: "append", localImageDelivery: "hosting" }); });
    expect(mediaHosting.publish).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "移除本地图片 dual-image" }));
    expect(useVideoStore.getState().shots[0]).toMatchObject({ referenceImages: ["https://cdn.example/dual.png"], referenceLocalImages: [], referenceAssetIds: ["dual-image"] });
    expect(useVideoStore.getState().importedSources?.flatMap((record) => record.sourceMaterials)).toEqual(expect.arrayContaining([expect.objectContaining({ assetId: "dual-image", publishedUrl: "https://cdn.example/dual.png" })]));
  });

  it("does not publish when hosting is unconfigured or model slots are exceeded", async () => {
    useProjectStore.setState({ activeId: "project-a", projects: [project("project-a")] });
    useVideoStore.getState().load({ model: "model", mode: "text", aspectRatio: "16:9", resolution: "720p", images: [], videos: [], audios: [], shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 3 }] });
    const image = asset("local-image", "image", "project-a", "D:/local-image.png");
    useLibraryStore.setState({ assets: [image], tasks: [] });
    mediaHosting.get.mockResolvedValue({ endpoint: "", fileField: "file", urlField: "url", authMode: "bearer", hasToken: false, configured: false });
    const pickerVersion = picker.version;
    picker.onApply = null;
    render(<VideoPanel />);
    await waitFor(() => expect(picker.version).toBeGreaterThan(pickerVersion));
    await (picker.onApply as unknown as (input: unknown) => Promise<void>)({ action: "reference", referenceMode: "replace", entries: [{ entryType: "library_asset", asset: image }] });
    expect(mediaHosting.publish).not.toHaveBeenCalled();

    useVideoStore.getState().load({ model: "model", mode: "text", aspectRatio: "16:9", resolution: "720p", images: [], videos: [], audios: [], shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 3 }] });
    const tooMany = Array.from({ length: 5 }, (_, index) => asset(`overflow-${index}`, "image", "project-a", `D:/overflow-${index}.png`));
    useLibraryStore.setState({ assets: tooMany, tasks: [] });
    await expect((picker.onApply as unknown as (input: unknown) => Promise<void>)({ action: "reference", referenceMode: "replace", entries: tooMany.map((asset) => ({ entryType: "library_asset", asset })) })).rejects.toThrow(/槽位不足/);
    expect(mediaHosting.publish).not.toHaveBeenCalled();
  });

  it("keeps successful media cached when a partial publish fails and retries only failed sources", async () => {
    useProjectStore.setState({ activeId: "project-a", projects: [project("project-a")] });
    useVideoStore.getState().load({ model: "model", mode: "text", aspectRatio: "16:9", resolution: "720p", images: [], videos: [], audios: [], shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 3 }] });
    const image = asset("retry-image", "image", "project-a", "D:/retry.png");
    const video = asset("retry-video", "video", "project-a", "D:/retry.mp4");
    useLibraryStore.setState({ assets: [image, video], tasks: [] });
    mediaHosting.publish.mockResolvedValueOnce({ results: [
      { key: "asset:retry-image", url: "https://cdn.example/retry.png", sha256: "a" },
      { key: "asset:retry-video", error: "temporary failure" },
    ] }).mockResolvedValueOnce({ results: [{ key: "asset:retry-video", url: "https://cdn.example/retry.mp4", sha256: "b" }] });
    picker.onApply = null;
    render(<VideoPanel />);
    await waitFor(() => expect(picker.onApply).not.toBeNull());
    const input = { action: "reference", referenceMode: "replace", entries: [{ entryType: "library_asset", asset: image }, { entryType: "library_asset", asset: video }] };
    await expect((picker.onApply as unknown as (input: unknown) => Promise<void>)(input)).rejects.toThrow(/retry-video/);
    await (picker.onApply as unknown as (input: unknown) => Promise<void>)(input);
    expect(mediaHosting.publish.mock.calls[1][0]).toEqual({ projectId: "project-a", expectedEndpoint: "https://host.example/upload", sources: [{ assetId: "retry-video" }] });
    expect(useVideoStore.getState().shots[0]).toMatchObject({ referenceLocalImages: [expect.objectContaining({ assetId: "retry-image" })], referenceVideos: ["https://cdn.example/retry.mp4"] });
  });

  it("does not backfill hosted references after the active project changes", async () => {
    useProjectStore.setState({ activeId: "project-a", projects: [project("project-a"), project("project-b")] });
    useVideoStore.getState().load({ model: "model", mode: "text", aspectRatio: "16:9", resolution: "720p", images: [], videos: [], audios: [], shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 3 }] });
    const video = asset("switch-video", "video", "project-a", "D:/switch.mp4");
    useLibraryStore.setState({ assets: [video], tasks: [] });
    let finishPublish: ((value: { results: Array<{ key: string; url: string }> }) => void) | undefined;
    mediaHosting.publish.mockImplementation(() => new Promise((resolve) => { finishPublish = resolve; }));
    picker.onApply = null;
    render(<VideoPanel />);
    await waitFor(() => expect(picker.onApply).not.toBeNull());
    const applying = (picker.onApply as unknown as (input: unknown) => Promise<void>)({ action: "reference", referenceMode: "replace", entries: [{ entryType: "library_asset", asset: video }] });
    await waitFor(() => expect(finishPublish).toBeDefined());
    useProjectStore.setState({ activeId: "project-b" });
    finishPublish?.({ results: [{ key: "asset:switch-video", url: "https://cdn.example/switch.mp4" }] });
    await expect(applying).rejects.toThrow(/项目.*变化/);
    expect(useVideoStore.getState().shots[0].referenceImages).toBeUndefined();
  });
  it("accepts only public credential-free HTTPS reference URLs", () => {
    expect(isPublicHttpsUrl("https://cdn.example.com/reference.png")).toBe(true);
    expect(isPublicHttpsUrl("http://cdn.example.com/reference.png")).toBe(false);
    expect(isPublicHttpsUrl("https://localhost/reference.png")).toBe(false);
    expect(isPublicHttpsUrl("https://127.0.0.1/reference.png")).toBe(false);
    expect(isPublicHttpsUrl("https://192.168.1.10/reference.png")).toBe(false);
    expect(isPublicHttpsUrl("https://user:secret@cdn.example.com/reference.png")).toBe(false);
  });

  it("resolves only current-project image/video assets per shot and never turns textual anchors into provider media", () => {
    const first = createEmptyStoryboardShot([]);
    first.referenceStrategy = "first_frame";
    first.referenceAssetIds = ["image-current", "anchor-document", "image-other-project"];

    const second = createEmptyStoryboardShot([first]);
    second.referenceStrategy = "reference";
    second.referenceAssetIds = ["video-current", "image-current"];

    const imported = resolveImportedStoryboardShots(
      [first, second],
      [
        asset("image-current", "image", "project-a", "https://cdn.example/current.png"),
        asset("video-current", "video", "project-a", "https://cdn.example/current.mp4"),
        asset("anchor-document", "text", "project-a", "anchor.md"),
        asset("image-other-project", "image", "project-b", "https://cdn.example/other.png"),
      ],
      (projectId) => projectId === "project-a",
    );

    expect(imported).toHaveLength(2);
    expect(imported[0]).toMatchObject({
      id: "shot-1",
      referenceAssetIds: ["image-current", "anchor-document", "image-other-project"],
      referenceImages: ["https://cdn.example/current.png"],
      referenceVideos: [],
    });
    expect(imported[1]).toMatchObject({
      id: "shot-2",
      referenceAssetIds: ["video-current", "image-current"],
      referenceImages: ["https://cdn.example/current.png"],
      referenceVideos: ["https://cdn.example/current.mp4"],
    });
    expect(unresolvedShotReferenceIds(imported[0], [
      asset("image-current", "image", "project-a", "https://cdn.example/current.png"),
      asset("anchor-document", "text", "project-a", "anchor.md"),
      asset("image-other-project", "image", "project-b", "https://cdn.example/other.png"),
    ], (projectId) => projectId === "project-a")).toEqual(["anchor-document", "image-other-project"]);
  });

  it("keeps reference-mode duration options and the visible material summary scoped to the individual shot", () => {
    const capability: VideoModelCapability = {
      id: "model",
      label: "Model",
      modes: ["text", "reference"],
      minDurationS: 1,
      maxDurationS: 5,
      durationOptions: [1, 2, 3, 4, 5],
      resolutions: ["720p"],
      aspectRatios: ["16:9"],
      maxImages: 4,
      maxVideos: 1,
      maxAudios: 0,
      maxReferenceDurationS: 3,
      note: "",
    };

    expect(durationOptionsFor(capability, "text")).toEqual([1, 2, 3, 4, 5]);
    expect(durationOptionsFor(capability, "reference")).toEqual([1, 2, 3]);
    expect(shotReferenceSummary({
      id: "shot-2",
      shotNo: 2,
      prompt: "镜头",
      durationS: 3,
      referenceStrategy: "reference",
      referenceImages: ["https://cdn.example/a.png", "https://cdn.example/b.png"],
      referenceVideos: ["https://cdn.example/a.mp4"],
    })).toBe("逐镜参考 3 项（图片 2 · 视频 1）");
  });

  it("renders each shot's actual strategy, reference count, and only that strategy's supported durations", async () => {
    useProjectStore.setState({
      activeId: "project-a",
      projects: [{
        id: "project-a",
        name: "测试项目",
        description: "",
        storyStyle: "",
        artStyle: "",
        aspectRatio: "16:9",
        imageModel: "image-model",
        imageQuality: "high",
        videoModel: "model",
        videoResolution: "720p",
      }],
    });
    useVideoStore.getState().load({
      model: "model",
      mode: "text",
      aspectRatio: "16:9",
      resolution: "720p",
      images: [],
      videos: [],
      audios: [],
      shots: [
        {
          id: "shot-1",
          shotNo: 1,
          prompt: "多素材镜头",
          durationS: 3,
          referenceStrategy: "reference",
          referenceImages: ["https://cdn.example/a.png", "https://cdn.example/b.png"],
          referenceVideos: ["https://cdn.example/a.mp4"],
        },
        { id: "shot-2", shotNo: 2, prompt: "文生镜头", durationS: 5, referenceStrategy: "text" },
      ],
    });

    render(<VideoPanel />);

    await waitFor(() => expect(screen.getByLabelText("第 1 镜生成方式与参考素材").textContent).toBe("多素材参考 · 逐镜参考 3 项（图片 2 · 视频 1）"));
    expect(screen.getByLabelText("第 2 镜生成方式与参考素材").textContent).toBe("文生视频 · 逐镜参考 0 项");
    expect(Array.from(screen.getByLabelText("第 1 镜时长").querySelectorAll("option"), (option) => option.getAttribute("value"))).toEqual(["1", "2", "3"]);
    expect(Array.from(screen.getByLabelText("第 2 镜时长").querySelectorAll("option"), (option) => option.getAttribute("value"))).toEqual(["1", "2", "3", "4", "5"]);
  });

  it("does not let a normal import rewrite a reviewed production manifest", async () => {
    const shot = createEmptyStoryboardShot([]);
    shot.referenceStrategy = "first_frame";
    shot.referenceAssetIds = ["image-current", "anchor-document", "image-other-project"];
    const source = asset("storyboard", "text", "project-a", "storyboard.md");
    source.params = { text: serializeStoryboard([shot]), documentType: "storyboard" };
    useProjectStore.setState({
      activeId: "project-a",
      projects: [{
        id: "project-a", name: "测试项目", description: "", storyStyle: "", artStyle: "", aspectRatio: "16:9",
        imageModel: "image-model", imageQuality: "high", videoModel: "model", videoResolution: "720p",
      }],
    });
    useLibraryStore.setState({
      assets: [
        source,
        asset("image-current", "image", "project-a", "https://cdn.example/current.png"),
        asset("anchor-document", "text", "project-a", "anchor.md"),
        asset("image-other-project", "image", "project-b", "https://cdn.example/other.png"),
      ],
      tasks: [],
    });
    useVideoStore.getState().load({
      model: "model",
      mode: "text",
      aspectRatio: "16:9",
      resolution: "720p",
      images: [],
      videos: [],
      audios: [],
      storyboardSourceAssetId: "approved-storyboard",
      productionManifest: {
        storyboardAssetId: "approved-storyboard",
        approvedModel: "model",
        approvedAspectRatio: "16:9",
        approvedResolution: "720p",
        approvedAt: 1,
        shots: [{ shotNo: 1, prompt: "旧镜头", durationS: 1 }],
      },
      shots: [{ id: "old", shotNo: 1, prompt: "旧镜头", durationS: 1 }],
    });

    render(<VideoPanel />);
    await waitFor(() => expect(picker.onApply).not.toBeNull());
    await expect((picker.onApply as unknown as (input: unknown) => Promise<void>)({ action: "prompt", referenceMode: "replace", entries: [{ entryType: "library_asset", asset: source }] })).rejects.toThrow(/已加载审查生产清单/);
    expect(useVideoStore.getState().storyboardSourceAssetId).toBe("approved-storyboard");
    expect(productionManifestMismatch(useVideoStore.getState())).toBeNull();
  });

  it("replacing prompt imports for shot 2 keeps shot 1's import record", async () => {
    useProjectStore.setState({ activeId: "project-a", projects: [project("project-a")] });
    const first = asset("prompt-one", "text", "project-a"); first.params = { text: "第一镜新提示词" };
    const second = asset("prompt-two", "text", "project-a"); second.params = { text: "第二镜新提示词" };
    useLibraryStore.setState({ assets: [first, second], tasks: [] });
    useVideoStore.getState().load({ model: "model", mode: "text", aspectRatio: "16:9", resolution: "720p", images: [], videos: [], audios: [], shots: [
      { id: "shot-1", shotNo: 1, prompt: "一", durationS: 3 }, { id: "shot-2", shotNo: 2, prompt: "二", durationS: 3 },
    ] });
    render(<VideoPanel />);
    await waitFor(() => expect(picker.onApply).not.toBeNull());
    act(() => picker.onApply?.({ action: "prompt", referenceMode: "replace", entries: [{ entryType: "library_asset", asset: first }] }));
    fireEvent.change(screen.getByLabelText("导入目标镜头"), { target: { value: "shot-2" } });
    act(() => picker.onApply?.({ action: "prompt", referenceMode: "replace", entries: [{ entryType: "library_asset", asset: second }] }));
    expect(useVideoStore.getState().importedSources).toEqual(expect.arrayContaining([
      expect.objectContaining({ targetShotId: "shot-1", assetIds: ["prompt-one"] }),
      expect.objectContaining({ targetShotId: "shot-2", assetIds: ["prompt-two"] }),
    ]));
  });

  it("fails closed when the provider lists a model without a verified local capability", async () => {
    useProjectStore.setState({
      activeId: "project-a",
      projects: [{
        id: "project-a", name: "测试项目", description: "", storyStyle: "", artStyle: "", aspectRatio: "16:9",
        imageModel: "image-model", imageQuality: "high", videoModel: "wan3-720p", videoResolution: "720p",
      }],
    });
    useVideoStore.getState().load({
      model: "wan3-720p", mode: "text", aspectRatio: "16:9", resolution: "720p",
      images: [], videos: [], audios: [], shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 5 }],
    });

    render(<VideoPanel />);

    await waitFor(() => expect(screen.getByText(/没有 wan3-720p 的能力定义/)).toBeTruthy());
    expect((screen.getByRole("button", { name: /生成 1 个视频镜头/ }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("renders shot reference videos and clicking remove button deletes it and resets referenceStrategy", async () => {
    useProjectStore.setState({ activeId: "project-a", projects: [project("project-a")] });
    useVideoStore.getState().load({
      model: "model",
      mode: "text",
      aspectRatio: "16:9",
      resolution: "720p",
      images: [],
      videos: [],
      audios: [],
      shots: [{
        id: "shot-1",
        shotNo: 1,
        prompt: "镜头",
        durationS: 3,
        referenceStrategy: "reference",
        referenceVideos: ["https://cdn.example/shot-video.mp4"],
      }],
    });

    render(<VideoPanel />);
    await waitFor(() => expect(screen.getByText("托管视频 URL · https://cdn.example/shot-video.mp4")).toBeTruthy());
    const removeBtn = screen.getByRole("button", { name: "移除托管视频 https://cdn.example/shot-video.mp4" });
    fireEvent.click(removeBtn);

    expect(useVideoStore.getState().shots[0].referenceVideos).toEqual([]);
    expect(useVideoStore.getState().shots[0].referenceStrategy).toBeUndefined();
    await waitFor(() => expect(screen.queryByText("托管视频 URL · https://cdn.example/shot-video.mp4")).toBeNull());
  });

  it("allows importing unassigned video references under default project", async () => {
    useProjectStore.setState({ activeId: "default-p", projects: [project("default-p")] });
    useVideoStore.getState().load({
      model: "model",
      mode: "text",
      aspectRatio: "16:9",
      resolution: "720p",
      images: [],
      videos: [],
      audios: [],
      shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 3 }],
    });
    const unassignedVideo = asset("unassigned-video", "video", "", "https://cdn.example/unassigned.mp4");
    useLibraryStore.setState({ assets: [unassignedVideo], tasks: [] });

    render(<VideoPanel />);
    await waitFor(() => expect(picker.onApply).not.toBeNull());
    await act(async () => {
      await picker.onApply?.({
        action: "reference",
        referenceMode: "replace",
        entries: [{ entryType: "library_asset", asset: unassignedVideo }],
      });
    });

    expect(useVideoStore.getState().shots[0]).toMatchObject({
      referenceVideos: ["https://cdn.example/unassigned.mp4"],
    });
  });

  it("refreshes the video catalogue on demand and accepts a hand-typed model", async () => {
    useProjectStore.setState({ activeId: "project-a", projects: [project("project-a")] });
    useVideoStore.getState().load({
      model: "model",
      mode: "text",
      aspectRatio: "16:9",
      resolution: "720p",
      images: [],
      videos: [],
      audios: [],
      shots: [{ id: "shot-1", shotNo: 1, prompt: "镜头", durationS: 3 }],
    });
    useLibraryStore.setState({ assets: [], tasks: [] });
    modelCatalog.fetch.mockResolvedValue(["model", "brand-new-video-model"]);

    render(<VideoPanel />);
    await waitFor(() => expect(screen.getByRole("combobox", { name: "视频模型" })).toBeTruthy());

    // No credentials in the panel: the backend reuses the saved video config.
    fireEvent.click(screen.getByRole("button", { name: "获取模型（视频模型）" }));
    await waitFor(() => {
      expect(modelCatalog.fetch).toHaveBeenCalledWith({ url: "", key: "", kind: "video" });
    });

    fireEvent.click(screen.getByRole("button", { name: "展开视频模型列表" }));
    await waitFor(() => {
      expect(screen.queryByRole("option", { name: "brand-new-video-model" })).toBeTruthy();
    });
    fireEvent.click(screen.getByRole("option", { name: "brand-new-video-model" }));
    expect(useVideoStore.getState().model).toBe("brand-new-video-model");

    fireEvent.change(screen.getByRole("combobox", { name: "视频模型" }), {
      target: { value: "自填视频模型" },
    });
    expect(useVideoStore.getState().model).toBe("自填视频模型");
    expect(screen.getByText(/已获取 2 个模型/)).toBeTruthy();
  });
});
