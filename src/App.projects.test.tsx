// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { appInfo, configStatus, listProviders, logsDir, markHistoryListenerReady } from "./lib/ipc";
import { dbExecute, dbSelect, getDb } from "./lib/db";
import { applyActive, loadSettings } from "./lib/settings";
import { loadHistory } from "./lib/dbWrite";
import { openPath } from "./lib/desktop";
import { confirmAction } from "./lib/confirm";
import { buildReviewedVideoHandoff } from "./lib/video/handoff";
import { useGenerationStore } from "./store/useGenerationStore";
import { useVideoStore } from "./store/useVideoStore";
import { useProjectStore, type Project } from "./store/useProjectStore";
import { resetProjectDrafts } from "./store/projectDrafts";
import App from "./App";

vi.mock("./lib/ipc", () => ({
  appInfo: vi.fn(),
  configStatus: vi.fn(),
  listProviders: vi.fn(),
  acknowledgeHistoryRevision: vi.fn(),
  markHistoryListenerReady: vi.fn(),
  logsDir: vi.fn(),
}));
vi.mock("./lib/db", () => ({ getDb: vi.fn(), dbExecute: vi.fn(), dbSelect: vi.fn() }));
vi.mock("./lib/settings", () => ({ loadSettings: vi.fn(), applyActive: vi.fn() }));
vi.mock("./lib/dbWrite", () => ({ loadHistory: vi.fn(), refreshLibraryHistory: vi.fn() }));
vi.mock("./lib/desktop", () => ({ listen: vi.fn(async () => () => {}), openPath: vi.fn(), openUrl: vi.fn() }));
vi.mock("./lib/confirm", () => ({ confirmAction: vi.fn() }));
vi.mock("./lib/logger", () => ({ logEvent: vi.fn() }));
vi.mock("./lib/video/handoff", () => ({ buildReviewedVideoHandoff: vi.fn() }));
vi.mock("./store/usePromptlibStore", () => ({ usePromptlibStore: { getState: () => ({ load: async () => {} }) } }));
vi.mock("./store/useAgentStore", () => ({ useAgentStore: { getState: () => ({ load: async () => {} }) } }));
// Pages and dialogs are covered by their own tests; App only routes to them.
vi.mock("./pages/GeneratePanel", () => ({ default: () => <div>图像生成面板</div> }));
vi.mock("./pages/VideoPanel", () => ({ default: () => <div>视频生成面板</div> }));
vi.mock("./pages/AssetsPage", () => ({ default: () => null }));
vi.mock("./pages/NovelComicPage", () => ({ default: () => null }));
vi.mock("./components/video/VideoMarkdownWorkspace", () => ({
  default: ({ onSendToVideo }: { onSendToVideo: (...args: unknown[]) => void }) => (
    <button type="button" onClick={() => onSendToVideo([], { asset: { id: "storyboard", kind: "text", path: "C:/s.md" }, source: "分镜", createdAt: 1 }, { approvedModel: "video-a", approvedAspectRatio: "16:9", approvedResolution: "720p" })}>
      发送当前分镜到视频生成
    </button>
  ),
}));
vi.mock("./components/SettingsPage", () => ({ default: () => null }));
vi.mock("./components/PromptManager", () => ({ default: () => null }));
vi.mock("./components/ProjectSettingsPage", () => ({ default: () => null }));
vi.mock("./components/AppDocsDialog", () => ({ default: () => null }));

const PROJECTS: Project[] = [
  { id: "p1", name: "项目一", description: "", storyStyle: "", artStyle: "", aspectRatio: "16:9", imageModel: "model-a", imageQuality: "high", videoModel: "video-a", videoResolution: "720p" },
  { id: "p2", name: "项目二", description: "", storyStyle: "", artStyle: "", aspectRatio: "9:16", imageModel: "model-b", imageQuality: "medium", videoModel: "video-b", videoResolution: "1080p" },
];

const projectSelect = () => screen.getByRole("combobox") as HTMLSelectElement;

async function renderLoaded() {
  render(<App />);
  await screen.findByText("就绪");
  await waitFor(() => expect(projectSelect().value).toBe("p1"));
}

async function switchTo(id: string) {
  fireEvent.change(projectSelect(), { target: { value: id } });
  await waitFor(() => expect(useProjectStore.getState().activeId).toBe(id));
  // The forms move right after the store switch resolves.
  await act(async () => {});
}

beforeEach(() => {
  resetProjectDrafts();
  useProjectStore.setState({ projects: [], activeId: null });
  useGenerationStore.getState().reset();
  useVideoStore.getState().load({});
  vi.mocked(appInfo).mockResolvedValue({ name: "Image-Client", version: "0.3.0", platform: "windows" });
  vi.mocked(listProviders).mockResolvedValue([]);
  vi.mocked(configStatus).mockResolvedValue({ imageReady: true, videoReady: true, llmReady: true, imageModel: "gpt-image-2", videoModel: "video", llmModel: "llm", outputDir: "", source: "db" });
  vi.mocked(markHistoryListenerReady).mockResolvedValue(undefined as never);
  vi.mocked(getDb).mockResolvedValue(undefined as never);
  vi.mocked(loadSettings).mockResolvedValue({} as never);
  vi.mocked(applyActive).mockResolvedValue(null as never);
  vi.mocked(loadHistory).mockResolvedValue({ assets: [], tasks: [] } as never);
  vi.mocked(dbExecute).mockResolvedValue(undefined as never);
  vi.mocked(dbSelect).mockImplementation((async (_sql: string, params?: unknown[]) => {
    if (params?.[0] === "projects") return [{ value: JSON.stringify(PROJECTS) }];
    if (params?.[0] === "active_project") return [{ value: "p1" }];
    return [];
  }) as never);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("App project drafts", () => {
  it("keeps each project's prompt, references and video shots across a round trip", async () => {
    await renderLoaded();
    expect(useGenerationStore.getState().model).toBe("model-a");

    useGenerationStore.getState().set({
      prompt: "项目一的提示词",
      model: "手填模型",
      referencePath: "C:/a.png",
      references: [{ path: "C:/a.png", role: "base_image", sortOrder: 0 }],
    });
    useVideoStore.getState().set({ shots: [{ id: "shot-1", shotNo: 1, prompt: "项目一的镜头", durationS: 5 }], mode: "reference", images: ["https://example.com/a.png"] });

    await switchTo("p2");
    // A project without a stash starts from its profile with empty drafts.
    expect(useGenerationStore.getState()).toMatchObject({ prompt: "", referencePath: "", references: [], model: "model-b", quality: "medium", size: "720x1280 (9:16)" });
    expect(useVideoStore.getState()).toMatchObject({ shots: [{ id: "shot-1", shotNo: 1, prompt: "", durationS: 5 }], model: "video-b", aspectRatio: "9:16", resolution: "1080p", mode: "text", images: [] });
    useGenerationStore.getState().set({ prompt: "项目二的提示词" });

    await switchTo("p1");
    expect(useGenerationStore.getState()).toMatchObject({ prompt: "项目一的提示词", model: "手填模型", referencePath: "C:/a.png", references: [{ path: "C:/a.png", role: "base_image", sortOrder: 0 }] });
    expect(useVideoStore.getState()).toMatchObject({ shots: [{ prompt: "项目一的镜头" }], mode: "reference", images: ["https://example.com/a.png"], model: "video-a" });

    await switchTo("p2");
    expect(useGenerationStore.getState().prompt).toBe("项目二的提示词");
  });

  it("gives a new project its profile defaults and keeps the previous project's draft", async () => {
    await renderLoaded();
    useGenerationStore.getState().set({ prompt: "项目一的提示词", references: [{ path: "C:/a.png", role: "base_image" }] });

    fireEvent.click(screen.getByTitle("新建项目"));
    await waitFor(() => expect(useProjectStore.getState().projects).toHaveLength(3));
    await act(async () => {});
    expect(useGenerationStore.getState()).toMatchObject({ prompt: "", references: [], model: "gpt-image-2", size: "1280x720 (16:9)" });
    expect(useVideoStore.getState().model).toBe("grok-imagine-video");

    await switchTo("p1");
    expect(useGenerationStore.getState()).toMatchObject({ prompt: "项目一的提示词", references: [{ path: "C:/a.png" }] });
  });

  it("restores the next project's draft after deleting the active one", async () => {
    await renderLoaded();
    await switchTo("p2");
    useGenerationStore.getState().set({ prompt: "项目二的提示词" });
    await switchTo("p1");
    useGenerationStore.getState().set({ prompt: "将被删除的提示词" });
    vi.mocked(confirmAction).mockResolvedValue(true);

    fireEvent.click(screen.getByTitle("删除项目"));

    await waitFor(() => expect(useProjectStore.getState().activeId).toBe("p2"));
    await act(async () => {});
    expect(useGenerationStore.getState()).toMatchObject({ prompt: "项目二的提示词", model: "model-b" });
  });

  it("shows a failed project operation instead of only logging it", async () => {
    await renderLoaded();
    vi.mocked(dbExecute).mockRejectedValue("磁盘已满");

    fireEvent.click(screen.getByTitle("新建项目"));

    expect((await screen.findByRole("status")).textContent).toBe("新建项目失败：磁盘已满");
  });
});

describe("App storyboard handoff", () => {
  beforeEach(() => {
    vi.mocked(buildReviewedVideoHandoff).mockReturnValue({ shots: [{ id: "shot-1", shotNo: 1, prompt: "分镜第一镜", durationS: 5 }], model: "video-a" });
  });

  it("asks before replacing written shots and keeps them on cancel", async () => {
    await renderLoaded();
    const drafts = [
      { id: "shot-1", shotNo: 1, prompt: "已写镜头一", durationS: 5 },
      { id: "shot-2", shotNo: 2, prompt: "已写镜头二", durationS: 5 },
      { id: "shot-3", shotNo: 3, prompt: "  ", durationS: 5 },
    ];
    useVideoStore.getState().set({ shots: drafts });
    fireEvent.click(screen.getByRole("button", { name: "短剧 Agent" }));

    vi.mocked(confirmAction).mockResolvedValue(false);
    fireEvent.click(await screen.findByRole("button", { name: "发送当前分镜到视频生成" }));
    await waitFor(() => expect(confirmAction).toHaveBeenCalledWith("视频生成页已有 2 个镜头草稿，将被当前分镜替换，继续？"));
    await act(async () => {});
    expect(useVideoStore.getState().shots).toEqual(drafts);
    expect(screen.queryByText("视频生成面板")).toBeNull();

    vi.mocked(confirmAction).mockResolvedValue(true);
    fireEvent.click(screen.getByRole("button", { name: "发送当前分镜到视频生成" }));
    expect(await screen.findByText("视频生成面板")).toBeTruthy();
    expect(useVideoStore.getState().shots).toEqual([{ id: "shot-1", shotNo: 1, prompt: "分镜第一镜", durationS: 5 }]);
  });

  it("sends without asking when the video form has no written shots", async () => {
    await renderLoaded();
    fireEvent.click(screen.getByRole("button", { name: "短剧 Agent" }));

    fireEvent.click(await screen.findByRole("button", { name: "发送当前分镜到视频生成" }));

    expect(await screen.findByText("视频生成面板")).toBeTruthy();
    expect(confirmAction).not.toHaveBeenCalled();
    expect(useVideoStore.getState().shots[0].prompt).toBe("分镜第一镜");
  });
});

describe("App startup failure", () => {
  it("explains a failed startup load and retries it", async () => {
    vi.mocked(getDb).mockRejectedValueOnce("数据库文件被占用");
    vi.mocked(logsDir).mockResolvedValue("C:/logs");
    render(<App />);

    const banner = await screen.findByRole("alert");
    expect(banner.textContent).toContain("数据库或设置加载失败：数据库文件被占用");
    expect(screen.getByText("加载失败")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "打开日志" }));
    await waitFor(() => expect(openPath).toHaveBeenCalledWith("C:/logs"));

    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText("就绪")).toBeTruthy();
    expect(getDb).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("alert")).toBeNull();
    expect(projectSelect().value).toBe("p1");
  });

  it("can dismiss the banner while the status bar still reports the failure", async () => {
    vi.mocked(getDb).mockRejectedValue("数据库文件被占用");
    render(<App />);

    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "关闭启动错误提示" }));

    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText("加载失败")).toBeTruthy();
  });
});
