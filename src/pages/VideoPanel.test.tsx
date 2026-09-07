// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import type { LibAsset } from "../store/useLibraryStore";
import { createEmptyStoryboardShot, serializeStoryboard } from "../lib/video/storyboard";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { productionManifestMismatch, useVideoStore } from "../store/useVideoStore";
import type { VideoModelCapability } from "../lib/ipc";
import { isPublicHttpsUrl } from "../lib/video/referenceUrl";

vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (path: string) => path }));
vi.mock("@tauri-apps/plugin-opener", () => ({ revealItemInDir: vi.fn() }));
vi.mock("../lib/generateVideo", () => ({ concatVideoAssets: vi.fn(), generateVideo: vi.fn() }));
const picker = vi.hoisted(() => ({ onPick: null as ((asset: unknown) => void) | null }));
vi.mock("../components/AssetPicker", () => ({
  default: ({ onPick }: { onPick: (asset: unknown) => void }) => {
    picker.onPick = onPick;
    return null;
  },
}));
vi.mock("../lib/ipc", () => ({
  listVideoModels: vi.fn().mockResolvedValue(["model"]),
  listVideoModelCapabilities: vi.fn().mockResolvedValue([{
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
  }]),
}));

import VideoPanel, {
  durationOptionsFor,
  resolveImportedStoryboardShots,
  shotReferenceSummary,
  unresolvedShotReferenceIds,
} from "./VideoPanel";

afterEach(() => {
  cleanup();
  useLibraryStore.setState({ assets: [], tasks: [] });
});

function asset(id: string, kind: "image" | "video" | "text", projectId: string, path = `https://cdn.example/${id}`): LibAsset {
  return { asset: { id, kind, path }, source: id, projectId, createdAt: 1 };
}

describe("VideoPanel storyboard import", () => {
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

  it("wires AssetPicker Markdown import to current-project media resolution without admitting anchor text or another project's asset", async () => {
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
    await waitFor(() => expect(picker.onPick).not.toBeNull());
    act(() => picker.onPick?.(source));

    expect(useVideoStore.getState().shots).toMatchObject([{
      id: "shot-1",
      referenceAssetIds: ["image-current", "anchor-document", "image-other-project"],
      referenceImages: ["https://cdn.example/current.png"],
      referenceVideos: [],
    }]);
    expect(useVideoStore.getState().storyboardSourceAssetId).toBe("storyboard");
    expect(productionManifestMismatch(useVideoStore.getState())).toBe("当前分镜来源已变化");
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
});
