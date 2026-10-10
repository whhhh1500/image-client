// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { resetViewState } from "../store/useViewStore";

const api = vi.hoisted(() => ({ novels: vi.fn(), comics: vi.fn() }));
vi.mock("../lib/novel/api", () => ({ novelWorkList: api.novels }));
vi.mock("../lib/comic/markdownApi", () => ({ comicMdCatalogList: api.comics }));
vi.mock("../components/novel/NovelAssetManager", () => ({ default: () => null }));
vi.mock("../lib/dbWrite", () => ({ refreshLibraryHistory: vi.fn().mockResolvedValue({ assets: [], tasks: [] }), markTaskRetried: vi.fn() }));
vi.mock("../lib/desktop", () => ({ fileSrc: (path: string) => path, openDialog: vi.fn(), revealItemInDir: vi.fn() }));

import AssetsPage from "./AssetsPage";

const videos = (count: number): LibAsset[] => Array.from({ length: count }, (_, index) => ({
  asset: { id: `video-${index}`, kind: "video", path: `C:/v${index}.mp4` },
  source: `视频 ${index}`,
  projectId: "project-a",
  createdAt: 1_000 - index,
}));
const image: LibAsset = { asset: { id: "image-a", kind: "image", path: "C:/a.png" }, source: "普通图片", projectId: "project-a", createdAt: 2_000 };
const page = () => render(<AssetsPage onQueueImport={vi.fn()} />);
const shownVideos = () => document.querySelectorAll("video").length;
const search = () => screen.getByPlaceholderText("搜索标题、正文、模型或提示词") as HTMLInputElement;

beforeEach(() => {
  vi.clearAllMocks();
  resetViewState();
  api.novels.mockResolvedValue([]);
  api.comics.mockResolvedValue([]);
  useProjectStore.setState({ activeId: "project-a", projects: [] });
  useLibraryStore.setState({ assets: [image, ...videos(130)], tasks: [] });
});
afterEach(() => cleanup());

describe("AssetsPage filters across a tab switch", () => {
  it("keeps the search, media and scope filters when the page is mounted again", () => {
    const first = page();
    fireEvent.click(screen.getByRole("button", { name: "视频" }));
    fireEvent.change(search(), { target: { value: "视频 1" } });
    fireEvent.click(screen.getByRole("button", { name: "未归属资源" }));
    first.unmount();

    page();
    expect(search().value).toBe("视频 1");
    expect(screen.getByText("没有未归属资源")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "当前项目" }));
    // "视频 1", "视频 10"–"视频 19" and "视频 100"–"视频 129"; the image stays filtered out.
    expect(shownVideos()).toBe(41);
    expect(screen.queryByText("普通图片")).toBeNull();
  });

  it("keeps the category tab", () => {
    const first = page();
    fireEvent.click(screen.getByRole("button", { name: /^外部上传/ }));
    expect(screen.getByText("没有匹配的资源")).toBeTruthy();
    first.unmount();

    page();
    expect(screen.getByText("没有匹配的资源")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /^全部\s*\d+$/ }));
    expect(shownVideos()).toBe(59);
  });

  it("still resets the revealed cards when a remembered filter changes", () => {
    const first = page();
    fireEvent.click(screen.getByRole("button", { name: "视频" }));
    first.unmount();

    page();
    expect(shownVideos()).toBe(60);
    fireEvent.click(screen.getByRole("button", { name: "加载更多（剩余 70）" }));
    expect(shownVideos()).toBe(120);

    fireEvent.click(screen.getByRole("button", { name: "全部" }));
    expect(shownVideos()).toBe(59);
    expect(screen.getByRole("button", { name: "加载更多（剩余 71）" })).toBeTruthy();
  });
});

describe("AssetsPage comic preview", () => {
  it("opens as a named dialog and closes with Escape", async () => {
    api.comics.mockResolvedValue([{ sourceUri: "comic://project-a/1", projectId: "project-a", kind: "text", novelWorkId: "work", novelChapterId: "chapter", chapterNo: 1, chapterTitle: "第一章", documentKind: "page_prompt", pageNo: 1, title: "第1页提示词", text: "页面正文", promptSnapshotComplete: true, createdAt: 1, stale: false }]);
    page();
    fireEvent.click((await screen.findByText("第1页提示词")).closest("button")!);

    const dialog = screen.getByRole("dialog", { name: "第1页提示词" });
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "关闭" }));

    fireEvent.keyDown(document.body, { key: "Escape" });

    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
