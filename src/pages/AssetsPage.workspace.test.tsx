// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";

const api = vi.hoisted(() => ({ novels: vi.fn(), comics: vi.fn() }));
vi.mock("../lib/novel/api", () => ({ novelWorkList: api.novels }));
vi.mock("../lib/comic/markdownApi", () => ({ comicMdCatalogList: api.comics }));
vi.mock("../components/novel/NovelAssetManager", () => ({ default: () => null }));
vi.mock("../lib/dbWrite", () => ({ refreshLibraryHistory: vi.fn().mockResolvedValue({ assets: [], tasks: [] }) }));
vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (path: string) => path }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ revealItemInDir: vi.fn() }));

import AssetsPage from "./AssetsPage";

const comic = (projectId: string, title: string) => ({ sourceUri: `comic://${projectId}/1`, projectId, kind: "text" as const, novelWorkId: "work", novelChapterId: "chapter", chapterNo: 1, chapterTitle: "第一章", documentId: "doc", documentRevision: 1, documentKind: "page_prompt", pageNo: 1, title, text: `${title}正文`, promptSnapshotComplete: true, createdAt: 1, stale: false });

describe("AssetsPage catalog behavior", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useProjectStore.setState({ activeId: "project-a", projects: [] });
    useLibraryStore.setState({ assets: [{ asset: { id: "image-a", kind: "image", path: "C:/a.png" }, source: "普通图片", projectId: "project-a", createdAt: 2 }], tasks: [] });
    api.novels.mockResolvedValue([{ id: "work", projectId: "project-a", title: "雾港", status: "active" }]);
    api.comics.mockResolvedValue([comic("project-a", "第1页")]);
  });
  afterEach(() => cleanup());

  it("renders ordinary, novel, and comic sources together in All with a real total", async () => {
    render(<AssetsPage onQueueImport={vi.fn()} />);
    await screen.findByText("雾港");
    expect(screen.getByText("第1页")).toBeTruthy();
    expect(screen.getByText("普通图片")).toBeTruthy();
    expect(screen.getByRole("button", { name: /全部 3/ })).toBeTruthy();
  });

  it("does not invent a zero chapter count when the source omits it", async () => {
    api.novels.mockResolvedValue([{ id: "work", projectId: "project-a", title: "雾港", status: "active" }]);
    render(<AssetsPage onQueueImport={vi.fn()} />);
    await screen.findByText("雾港");
    expect(screen.getByText("查看章节 · 进行中")).toBeTruthy();
    expect(screen.queryByText(/0 章/)).toBeNull();
  });

  it("clears a prior project's deferred source response before rendering the next project", async () => {
    let resolveOld!: (value: ReturnType<typeof comic>[]) => void;
    api.comics.mockImplementation(({ projectId }: { projectId: string }) => projectId === "project-a"
      ? new Promise((resolve) => { resolveOld = resolve; })
      : Promise.resolve([comic("project-b", "新项目第1页")]));
    api.novels.mockImplementation((projectId: string) => Promise.resolve([{ id: `work-${projectId}`, projectId, title: projectId === "project-a" ? "旧小说" : "新小说", status: "active" }]));
    render(<AssetsPage onQueueImport={vi.fn()} />);
    await screen.findByText("正在读取漫画资料…");
    useProjectStore.setState({ activeId: "project-b" });
    await screen.findByText("新小说");
    await screen.findByText("新项目第1页");
    resolveOld([comic("project-a", "旧项目第1页")]);
    await waitFor(() => expect(screen.queryByText("旧项目第1页")).toBeNull());
  });

  it("seeds only page prompts for a comic group and includes the novel work in its label", async () => {
    const queued = vi.fn();
    api.comics.mockResolvedValue([
      comic("project-a", "第1页提示词"),
      { ...comic("project-a", "故事梗概"), sourceUri: "comic://project-a/script", documentKind: "script", text: "剧情正文", pageNo: undefined },
      { ...comic("project-a", "第1页历史图"), sourceUri: "comic://project-a/image", kind: "image" as const, documentKind: undefined, text: undefined, effectivePrompt: "已渲染页面", path: "C:/page-1.png", pageNo: 1 },
    ]);
    render(<AssetsPage onQueueImport={queued} />);
    const heading = await screen.findByText("雾港 · 第1章 · 第一章 · 3 项");
    expect(screen.getByAltText("第1页历史图").getAttribute("src")).toBe("C:/page-1.png");
    const group = heading.parentElement!;
    await waitFor(() => expect(within(group).getByRole("button", { name: "用于生图" })).toBeTruthy());
    within(group).getByRole("button", { name: "用于生图" }).click();
    expect(queued).toHaveBeenCalledWith([
      expect.objectContaining({ sourceUri: "comic://project-a/1", documentKind: "page_prompt", kind: "text" }),
    ], "image", "merge_prompt");
  });

  it("queues upload groups as target-media references instead of prompt merges", async () => {
    const queued = vi.fn();
    useLibraryStore.setState({ assets: [
      { asset: { id: "upload-image", kind: "image", path: "C:/upload.png" }, source: "上传图片", projectId: "project-a", createdAt: 2, params: { catalog: { version: 1, category: "upload", origin: "local_upload", group: { type: "upload_batch", id: "batch" } } } },
      { asset: { id: "upload-video", kind: "video", path: "C:/upload.mp4" }, source: "上传视频", projectId: "project-a", createdAt: 1, params: { catalog: { version: 1, category: "upload", origin: "local_upload", group: { type: "upload_batch", id: "batch" } } } },
    ], tasks: [] });
    render(<AssetsPage onQueueImport={queued} />);
    const heading = await screen.findByText("外部上传批次 · 2 项");
    const group = heading.parentElement!;
    within(group).getByRole("button", { name: "用于生图" }).click();
    expect(queued).toHaveBeenLastCalledWith([
      expect.objectContaining({ asset: expect.objectContaining({ asset: expect.objectContaining({ id: "upload-image" }) }) }),
    ], "image", "reference");
    within(group).getByRole("button", { name: "用于视频" }).click();
    expect(queued).toHaveBeenLastCalledWith([
      expect.objectContaining({ asset: expect.objectContaining({ asset: expect.objectContaining({ id: "upload-video" }) }) }),
    ], "video", "reference");
  });

  it("falls back to image references when a comic group has no page prompt or document", async () => {
    const queued = vi.fn();
    api.comics.mockResolvedValue([{
      ...comic("project-a", "缺少提示词的页面图"),
      kind: "image" as const,
      sourceUri: "comic://project-a/image-only",
      documentKind: undefined,
      text: undefined,
      path: "C:/image-only.png",
      effectivePrompt: undefined,
    }]);
    render(<AssetsPage onQueueImport={queued} />);
    const heading = await screen.findByText("雾港 · 第1章 · 第一章 · 1 项");
    within(heading.parentElement!).getByRole("button", { name: "用于生图" }).click();
    expect(queued).toHaveBeenCalledWith([
      expect.objectContaining({ sourceUri: "comic://project-a/image-only", kind: "image", path: "C:/image-only.png" }),
    ], "image", "reference");
  });
});
