// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

const api = vi.hoisted(() => ({ list: vi.fn(), get: vi.fn(), createWork: vi.fn(), saveChapter: vi.fn() }));
vi.mock("../../lib/novel/api", () => ({
  novelWorkList: api.list,
  novelWorkGet: api.get,
  novelWorkCreate: api.createWork,
  novelChapterRevisionCreate: api.saveChapter,
  newNovelIdempotencyKey: (prefix: string) => `${prefix}:key`,
}));

import NovelAssetManager, { type NovelAssetSelection } from "./NovelAssetManager";

const chapterOne = { id: "chapter-1", novelWorkId: "work-1", chapterNo: 1, sequenceNo: 1, title: "雨夜来信", latestRevisionId: "revision-1" };
const revisionOne = { id: "revision-1", novelWorkId: "work-1", chapterId: "chapter-1", revisionNo: 2, content: "第一章的固定正文", assetId: undefined as string | undefined };
const snapshot = (chapters = [chapterOne], revisions = [revisionOne]) => ({ work: { id: "work-1", projectId: "project-1", title: "风灯", status: "active" }, chapters, revisions });

describe("NovelAssetManager", () => {
  const select = vi.fn();
  beforeEach(() => {
    vi.clearAllMocks();
    api.list.mockResolvedValue([{ id: "work-1", projectId: "project-1", title: "风灯", status: "active" }]);
    api.get.mockResolvedValue(snapshot());
  });
  afterEach(() => { vi.restoreAllMocks(); cleanup(); localStorage.clear(); });
  const open = () => render(<NovelAssetManager mode="select" projectId="project-1" open onClose={vi.fn()} onSelect={select} />);

  it("returns a concrete current chapter revision instead of a library asset", async () => {
    open();
    await screen.findByLabelText("章节正文预览");
    fireEvent.click(screen.getByRole("button", { name: "引用此章节原文" }));
    expect(select).toHaveBeenCalledWith({
      projectId: "project-1", novelWorkId: "work-1", workTitle: "风灯", novelChapterId: "chapter-1", novelChapterRevisionId: "revision-1", revisionNo: 2, chapterNo: 1, title: "雨夜来信", content: "第一章的固定正文",
    } satisfies NovelAssetSelection);
  });

  it("focuses the active short-drama chapter when reopened for browsing", async () => {
    const chapterTwo = { id: "chapter-2", novelWorkId: "work-1", chapterNo: 2, sequenceNo: 2, title: "第二章", latestRevisionId: "revision-2" };
    const revisionTwo = { id: "revision-2", novelWorkId: "work-1", chapterId: "chapter-2", revisionNo: 1, content: "当前短剧已引用的第二章", assetId: undefined };
    api.get.mockResolvedValue(snapshot([chapterOne, chapterTwo], [revisionOne, revisionTwo]));
    render(<NovelAssetManager mode="select" projectId="project-1" open onClose={vi.fn()} onSelect={select} initialSelection={{ projectId: "project-1", novelWorkId: "work-1", novelChapterId: "chapter-2" }} />);
    expect((await screen.findByLabelText("小说章节")) as HTMLSelectElement).toHaveProperty("value", "chapter-2");
    expect(screen.getByLabelText("章节正文预览").textContent).toContain("当前短剧已引用的第二章");
  });

  it("creates a separate next chapter, preserves existing chapters, and exposes the new fixed revision", async () => {
    api.saveChapter.mockResolvedValue({ id: "revision-2", novelWorkId: "work-1", chapterId: "chapter-2", revisionNo: 1, content: "第二章正文" });
    api.get
      .mockResolvedValueOnce(snapshot())
      .mockResolvedValueOnce(snapshot([
        chapterOne,
        { id: "chapter-2", novelWorkId: "work-1", chapterNo: 2, sequenceNo: 2, title: "第二章", latestRevisionId: "revision-2" },
      ], [revisionOne, { id: "revision-2", novelWorkId: "work-1", chapterId: "chapter-2", revisionNo: 1, content: "第二章正文", assetId: undefined }]));
    open();
    await screen.findByRole("button", { name: "添加新章节" });
    fireEvent.click(screen.getByRole("button", { name: "添加新章节" }));
    fireEvent.change(screen.getByLabelText("章节名称"), { target: { value: "第二章" } });
    fireEvent.change(screen.getByLabelText("小说章节正文"), { target: { value: "第二章正文" } });
    fireEvent.click(screen.getByRole("button", { name: "保存新章节" }));
    await waitFor(() => expect(api.saveChapter).toHaveBeenCalledWith(expect.objectContaining({ chapterId: undefined, chapterNo: 2, title: "第二章", content: "第二章正文" })));
    await screen.findByText("第二章正文");
    fireEvent.click(screen.getByRole("button", { name: "引用此章节原文" }));
    expect(select).toHaveBeenLastCalledWith(expect.objectContaining({ novelChapterId: "chapter-2", novelChapterRevisionId: "revision-2", content: "第二章正文" }));
  });

  it("has an actionable empty state and lets a new book continue to its first chapter", async () => {
    api.list.mockResolvedValue([]);
    api.createWork.mockResolvedValue({ id: "work-2", projectId: "project-1", title: "新书", status: "active" });
    api.get.mockResolvedValue({ work: { id: "work-2", projectId: "project-1", title: "新书", status: "active" }, chapters: [], revisions: [] });
    open();
    await screen.findByText(/当前项目还没有小说/);
    fireEvent.click(screen.getByRole("button", { name: "新建小说" }));
    fireEvent.change(screen.getByLabelText("新小说名称"), { target: { value: "新书" } });
    fireEvent.click(screen.getByRole("button", { name: "创建并添加第一章" }));
    await waitFor(() => expect(api.createWork).toHaveBeenCalled());
    expect(await screen.findByText(/新增 新书 的章节/)).toBeTruthy();
  });

  it("keeps an edit draft on a failed save and never selects it as a new source", async () => {
    api.saveChapter.mockRejectedValueOnce(new Error("本地数据库暂不可用"));
    open();
    await screen.findByRole("button", { name: "编辑并保存新修订" });
    fireEvent.click(screen.getByRole("button", { name: "编辑并保存新修订" }));
    fireEvent.change(screen.getByLabelText("章节名称"), { target: { value: "雨夜来信（修订）" } });
    fireEvent.change(screen.getByLabelText("小说章节正文"), { target: { value: "尚未保存的修订正文" } });
    fireEvent.click(screen.getByRole("button", { name: "保存新修订" }));
    expect((await screen.findByRole("alert")).textContent).toContain("保存章节修订失败");
    expect(screen.getByLabelText("章节名称")).toHaveProperty("value", "雨夜来信（修订）");
    expect(screen.getByLabelText("小说章节正文")).toHaveProperty("value", "尚未保存的修订正文");
    expect(select).not.toHaveBeenCalled();
  });

  it("keeps an unsaved new-chapter draft when the dialog is closed and reopened", async () => {
    const close = vi.fn();
    const view = render(<NovelAssetManager mode="select" projectId="project-1" open onClose={close} onSelect={select} />);
    await screen.findByRole("button", { name: "添加新章节" });
    fireEvent.click(screen.getByRole("button", { name: "添加新章节" }));
    fireEvent.change(screen.getByLabelText("小说章节正文"), { target: { value: "关闭前的章节草稿" } });
    view.rerender(<NovelAssetManager mode="select" projectId="project-1" open={false} onClose={close} onSelect={select} />);
    view.rerender(<NovelAssetManager mode="select" projectId="project-1" open onClose={close} onSelect={select} />);
    expect(await screen.findByLabelText("小说章节正文")).toHaveProperty("value", "关闭前的章节草稿");
    expect(api.saveChapter).not.toHaveBeenCalled();
  });

  it("blocks selection after a saved revision cannot be reread, then restores the saved revision on retry", async () => {
    const revisionTwo = { id: "revision-2", novelWorkId: "work-1", chapterId: "chapter-1", revisionNo: 3, content: "重新读取后的修订正文", assetId: undefined };
    api.saveChapter.mockResolvedValue(revisionTwo);
    api.get
      .mockResolvedValueOnce(snapshot())
      .mockRejectedValueOnce(new Error("暂时无法读取"))
      .mockResolvedValueOnce(snapshot([{
        ...chapterOne, latestRevisionId: "revision-2",
      }], [revisionOne, revisionTwo]));
    open();
    await screen.findByRole("button", { name: "编辑并保存新修订" });
    fireEvent.click(screen.getByRole("button", { name: "编辑并保存新修订" }));
    fireEvent.change(screen.getByLabelText("小说章节正文"), { target: { value: "重新读取后的修订正文" } });
    fireEvent.click(screen.getByRole("button", { name: "保存新修订" }));
    expect(await screen.findByText("章节尚未重新读取")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "引用此章节原文" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重新读取章节" }));
    await screen.findByText("重新读取后的修订正文");
    fireEvent.click(screen.getByRole("button", { name: "引用此章节原文" }));
    expect(select).toHaveBeenLastCalledWith(expect.objectContaining({ novelChapterRevisionId: "revision-2", revisionNo: 3, content: "重新读取后的修订正文" }));
  });

  it("recovers a saved revision after close and reopen instead of leaving its old editor disabled", async () => {
    const revisionTwo = { id: "revision-2", novelWorkId: "work-1", chapterId: "chapter-1", revisionNo: 3, content: "重开后确认的修订正文", assetId: undefined };
    api.saveChapter.mockResolvedValue(revisionTwo);
    api.get
      .mockResolvedValueOnce(snapshot())
      .mockRejectedValueOnce(new Error("首次刷新失败"))
      .mockResolvedValueOnce(snapshot([{ ...chapterOne, latestRevisionId: "revision-2" }], [revisionOne, revisionTwo]));
    const close = vi.fn();
    const view = render(<NovelAssetManager mode="select" projectId="project-1" open onClose={close} onSelect={select} />);
    await screen.findByRole("button", { name: "编辑并保存新修订" });
    fireEvent.click(screen.getByRole("button", { name: "编辑并保存新修订" }));
    fireEvent.change(screen.getByLabelText("小说章节正文"), { target: { value: "重开后确认的修订正文" } });
    fireEvent.click(screen.getByRole("button", { name: "保存新修订" }));
    await screen.findByText("章节尚未重新读取");
    view.rerender(<NovelAssetManager mode="select" projectId="project-1" open={false} onClose={close} onSelect={select} />);
    view.rerender(<NovelAssetManager mode="select" projectId="project-1" open onClose={close} onSelect={select} />);
    await screen.findByText("重开后确认的修订正文");
    expect(screen.getByRole("button", { name: "引用此章节原文" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "保存新修订" })).toBeNull();
  });

  it("does not let a mutation started in one project populate a newly selected project", async () => {
    let resolveSave!: (value: typeof revisionOne) => void;
    api.saveChapter.mockImplementationOnce(() => new Promise((resolve) => { resolveSave = resolve; }));
    const view = render(<NovelAssetManager mode="select" projectId="project-1" open onClose={vi.fn()} onSelect={select} />);
    await screen.findByRole("button", { name: "编辑并保存新修订" });
    fireEvent.click(screen.getByRole("button", { name: "编辑并保存新修订" }));
    fireEvent.change(screen.getByLabelText("小说章节正文"), { target: { value: "跨项目时仍在飞行的请求" } });
    fireEvent.click(screen.getByRole("button", { name: "保存新修订" }));
    api.list.mockResolvedValue([]);
    view.rerender(<NovelAssetManager mode="select" projectId="project-2" open onClose={vi.fn()} onSelect={select} />);
    expect(screen.getByRole("button", { name: "新建小说" })).toHaveProperty("disabled", false);
    resolveSave(revisionOne);
    await screen.findByText(/当前项目还没有小说/);
    expect(screen.queryByText("跨项目时仍在飞行的请求")).toBeNull();
    expect(select).not.toHaveBeenCalled();
  });

  it("does not let a late response for an older work replace the current selection", async () => {
    let resolveOld!: (value: ReturnType<typeof snapshot>) => void;
    api.list.mockResolvedValue([
      { id: "work-1", projectId: "project-1", title: "旧书", status: "active" },
      { id: "work-2", projectId: "project-1", title: "新书", status: "active" },
    ]);
    api.get.mockImplementation(({ novelWorkId }: { novelWorkId: string }) => novelWorkId === "work-1"
      ? new Promise((resolve) => { resolveOld = resolve; })
      : Promise.resolve({ work: { id: "work-2", projectId: "project-1", title: "新书", status: "active" }, chapters: [{ id: "chapter-2", novelWorkId: "work-2", chapterNo: 1, title: "新章", latestRevisionId: "revision-2" }], revisions: [{ id: "revision-2", novelWorkId: "work-2", chapterId: "chapter-2", revisionNo: 1, content: "新书正文" }] }));
    open();
    await screen.findByLabelText("小说资产");
    fireEvent.change(screen.getByLabelText("小说资产"), { target: { value: "work-2" } });
    await screen.findByText("新书正文");
    resolveOld(snapshot());
    await waitFor(() => expect(screen.getByLabelText("章节正文预览").textContent).toContain("新书正文"));
  });

  it("restores a legacy comic draft only inside the editor, then clears it after the new revision is durable", async () => {
    const revisionTwo = { id: "revision-2", novelWorkId: "work-1", chapterId: "chapter-1", revisionNo: 3, content: "恢复的未保存正文", assetId: undefined };
    localStorage.setItem("comic-md:source:project-1:work-1:chapter-1", JSON.stringify({ chapterNo: 1, title: "恢复标题", content: revisionTwo.content }));
    api.saveChapter.mockResolvedValue(revisionTwo);
    api.get.mockResolvedValueOnce(snapshot()).mockResolvedValueOnce(snapshot([{ ...chapterOne, latestRevisionId: revisionTwo.id }], [revisionTwo]));
    const changed = vi.fn();
    render(<NovelAssetManager mode="manage" projectId="project-1" open onClose={vi.fn()} onChanged={changed} />);
    await screen.findByRole("button", { name: "编辑并保存新修订" });
    fireEvent.click(screen.getByRole("button", { name: "编辑并保存新修订" }));
    expect((await screen.findByRole("status")).textContent).toContain("已恢复未保存的小说原文草稿");
    expect(screen.getByLabelText("章节名称")).toHaveProperty("value", "恢复标题");
    expect(screen.getByLabelText("小说章节正文")).toHaveProperty("value", revisionTwo.content);
    expect(screen.queryByRole("button", { name: "引用此章节原文" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "保存新修订" }));
    await waitFor(() => expect(changed).toHaveBeenCalledWith({ projectId: "project-1", novelWorkId: "work-1", novelChapterId: "chapter-1", novelChapterRevisionId: "revision-2" }));
    expect(localStorage.getItem("comic-md:source:project-1:work-1:chapter-1")).toBeNull();
    expect(localStorage.getItem("novel-asset:source:project-1:work-1:chapter-1")).toBeNull();
  });

  it("does not report a durable save as failed or preserve its draft when a consumer refresh callback throws", async () => {
    const revisionTwo = { id: "revision-2", novelWorkId: "work-1", chapterId: "chapter-1", revisionNo: 3, content: "持久保存后的正文", assetId: undefined };
    api.saveChapter.mockResolvedValue(revisionTwo);
    api.get.mockResolvedValueOnce(snapshot()).mockResolvedValueOnce(snapshot([{ ...chapterOne, latestRevisionId: revisionTwo.id }], [revisionTwo]));
    localStorage.setItem("novel-asset:source:project-1:work-1:chapter-1", JSON.stringify({ chapterNo: 1, title: "雨夜来信", content: revisionTwo.content }));
    render(<NovelAssetManager mode="manage" projectId="project-1" open onClose={vi.fn()} onChanged={() => { throw new Error("consumer refresh failed"); }} />);
    await screen.findByRole("button", { name: "编辑并保存新修订" });
    fireEvent.click(screen.getByRole("button", { name: "编辑并保存新修订" }));
    fireEvent.click(screen.getByRole("button", { name: "保存新修订" }));
    await screen.findByText(/章节已保存，但使用方刷新通知失败/);
    expect(api.saveChapter).toHaveBeenCalledTimes(1);
    expect(localStorage.getItem("novel-asset:source:project-1:work-1:chapter-1")).toBeNull();
  });

  it("never writes a prior project's editor draft under the next project key", async () => {
    const view = render(<NovelAssetManager mode="select" projectId="project-1" open onClose={vi.fn()} onSelect={select} />);
    await screen.findByRole("button", { name: "编辑并保存新修订" });
    fireEvent.click(screen.getByRole("button", { name: "编辑并保存新修订" }));
    await waitFor(() => expect(localStorage.getItem("novel-asset:source:project-1:work-1:chapter-1")).not.toBeNull());
    api.list.mockResolvedValue([]);
    view.rerender(<NovelAssetManager mode="select" projectId="project-2" open onClose={vi.fn()} onSelect={select} />);
    await screen.findByText(/当前项目还没有小说/);
    expect(localStorage.getItem("novel-asset:source:project-2:work-1:chapter-1")).toBeNull();
  });

  it("warns before a close or refresh when the local editor draft cannot be persisted", async () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("storage full"); });
    render(<NovelAssetManager mode="manage" projectId="project-1" open onClose={vi.fn()} />);
    await screen.findByRole("button", { name: "编辑并保存新修订" });
    fireEvent.click(screen.getByRole("button", { name: "编辑并保存新修订" }));
    expect((await screen.findByText(/本地草稿暂时无法保存/)).textContent).toContain("关闭或刷新前请先复制正文");
  });
});
