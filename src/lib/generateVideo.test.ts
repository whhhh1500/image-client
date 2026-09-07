import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (path: string) => path }));
vi.mock("./ipc", () => ({ runVideo: vi.fn(), saveMediaAsset: vi.fn() }));
vi.mock("./dbWrite", () => ({ persistAssets: vi.fn(), persistTask: vi.fn() }));
vi.mock("./logger", () => ({ logEvent: vi.fn() }));
vi.mock("./media", () => ({ concatMp4: vi.fn() }));

import { persistAssets, persistTask } from "./dbWrite";
import { generateVideo } from "./generateVideo";
import { runVideo, saveMediaAsset } from "./ipc";
import { concatMp4 } from "./media";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";

const params = {
  shots: [
    { id: "shot-1", shotNo: 1, prompt: "镜头一", durationS: 3 },
    { id: "shot-2", shotNo: 2, prompt: "镜头二", durationS: 3 },
  ],
  model: "grok-imagine-video",
  aspectRatio: "16:9",
  resolution: "480p",
  mode: "text" as const,
  images: [],
  videos: [],
  audios: [],
};

describe("generateVideo independent shots", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useProjectStore.setState({ activeId: "project_video" });
    useLibraryStore.setState({ assets: [], tasks: [] });
    vi.mocked(persistTask).mockResolvedValue();
    vi.mocked(persistAssets).mockResolvedValue();
    vi.mocked(runVideo)
      .mockResolvedValueOnce({ assets: [{ id: "segment_1", kind: "video", path: "C:/1.mp4", durationS: 3 }] })
      .mockResolvedValueOnce({ assets: [{ id: "segment_2", kind: "video", path: "C:/2.mp4", durationS: 3 }] });
  });

  it("submits and persists every prompt as an independent resource without concatenating", async () => {
    await expect(generateVideo(params)).resolves.toHaveLength(2);
    expect(runVideo).toHaveBeenCalledTimes(2);
    expect(vi.mocked(runVideo).mock.calls.map(([request]) => request.config)).toEqual([
      expect.objectContaining({ prompt: "镜头一", duration_s: 3 }),
      expect.objectContaining({ prompt: "镜头二", duration_s: 3 }),
    ]);
    expect(persistAssets).toHaveBeenCalledTimes(2);
    expect(persistAssets).toHaveBeenNthCalledWith(1, [expect.objectContaining({ id: "segment_1" })], "视频镜头 1/2", expect.anything());
    expect(persistAssets).toHaveBeenNthCalledWith(2, [expect.objectContaining({ id: "segment_2" })], "视频镜头 2/2", expect.anything());
    expect(concatMp4).not.toHaveBeenCalled();
    expect(saveMediaAsset).not.toHaveBeenCalled();
    expect(useLibraryStore.getState().assets.map((item) => item.asset.id)).toEqual(["segment_2", "segment_1"]);
  });

  it("keeps the source storyboard as parent lineage on every generated shot", async () => {
    useLibraryStore.setState({
      assets: [{
        asset: { id: "storyboard_source", kind: "text", path: "C:/storyboard.md" },
        source: "视频分镜",
        projectId: "project_video",
        params: { text: "分镜正文", documentType: "storyboard" },
        createdAt: 1,
      }],
      tasks: [],
    });

    await generateVideo({ ...params, storyboardSourceAssetId: "storyboard_source" });

    expect(persistAssets).toHaveBeenNthCalledWith(1, expect.anything(), "视频镜头 1/2", expect.objectContaining({
      params: expect.objectContaining({
        provenance: expect.objectContaining({ parentAssetIds: ["storyboard_source"] }),
      }),
    }));
    expect(persistAssets).toHaveBeenNthCalledWith(2, expect.anything(), "视频镜头 2/2", expect.objectContaining({
      params: expect.objectContaining({
        provenance: expect.objectContaining({ parentAssetIds: ["storyboard_source"] }),
      }),
    }));
  });

  it("submits per-shot references without leaking them into text-only shots", async () => {
    const referenced = {
      ...params,
      shots: [
        { id: "shot-1", shotNo: 1, prompt: "首帧镜头", durationS: 3, referenceStrategy: "first_frame" as const, referenceAssetIds: ["image-a"], referenceImages: ["https://example.com/a.png"] },
        { id: "shot-2", shotNo: 2, prompt: "纯文本镜头", durationS: 3, referenceStrategy: "text" as const, referenceAssetIds: [] },
      ],
    };
    await generateVideo(referenced);
    expect(vi.mocked(runVideo).mock.calls.map(([request]) => request.config)).toEqual([
      expect.objectContaining({ mode: "first_frame", images: ["https://example.com/a.png"], videos: [] }),
      expect.objectContaining({ mode: "text", images: [], videos: [] }),
    ]);
    expect(persistAssets).toHaveBeenNthCalledWith(1, expect.anything(), "视频镜头 1/2", expect.objectContaining({ params: expect.objectContaining({ referenceStrategy: "first_frame", referenceAssetIds: ["image-a"] }) }));
    expect(persistAssets).toHaveBeenNthCalledWith(2, expect.anything(), "视频镜头 2/2", expect.objectContaining({ params: expect.objectContaining({ referenceStrategy: "text", referenceAssetIds: [] }) }));
  });

  it("rejects legacy audio references because sound work is outside the current video workspace", async () => {
    await expect(generateVideo({ ...params, audios: ["https://example.com/reference.mp3"] }))
      .rejects.toThrow("暂不处理声音或音频参考");
    expect(runVideo).not.toHaveBeenCalled();
    expect(persistTask).not.toHaveBeenCalled();
  });

  it("persists the provider task id carried by a zzone video asset", async () => {
    vi.mocked(runVideo).mockReset().mockResolvedValueOnce({
      assets: [{ id: "zzone:video_task_123", kind: "video", path: "C:/provider.mp4", durationS: 3 }],
    });
    await generateVideo({ ...params, shots: [params.shots[0]] });
    expect(persistAssets).toHaveBeenCalledWith(
      [expect.objectContaining({ id: "zzone:video_task_123" })],
      "视频镜头 1/1",
      expect.objectContaining({ params: expect.objectContaining({ providerTaskId: "video_task_123" }) }),
    );
  });

  it("rejects private reference URLs before creating a local or provider task", async () => {
    await expect(generateVideo({
      ...params,
      mode: "first_frame",
      images: ["https://127.0.0.1/reference.png"],
      shots: [{ ...params.shots[0], referenceStrategy: "first_frame" }],
    })).rejects.toThrow("不能使用本机或内网地址");
    expect(runVideo).not.toHaveBeenCalled();
    expect(persistTask).not.toHaveBeenCalled();
  });
});
