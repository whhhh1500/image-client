// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { MdWorkspace } from "../../lib/comic/markdownApi";
const api = vi.hoisted(() => ({ list: vi.fn(), get: vi.fn(), workspace: vi.fn(), save: vi.fn(), generate: vi.fn(), render: vi.fn(), history: vi.fn(), export: vi.fn(), optimize: vi.fn(), options: vi.fn(), sync: vi.fn(), visualSave: vi.fn(), visualExtract: vi.fn() }));
const manager = vi.hoisted(() => ({ props: null as unknown }));
vi.mock("../../store/useProjectStore", () => ({ useProjectStore: (selector: (state: { activeId: string }) => unknown) => selector({ activeId: "p" }) }));
vi.mock("../../lib/novel/api", () => ({ novelWorkList: api.list, novelWorkGet: api.get }));
vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (path: string) => path }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openPath: vi.fn(), revealItemInDir: vi.fn() }));
vi.mock("../../lib/comic/markdownApi", async (original) => ({ ...await original<typeof import("../../lib/comic/markdownApi")>(), comicMdWorkspaceGet: api.workspace, comicMdDocumentSave: api.save, comicMdGenerate: api.generate, comicMdRender: api.render, comicMdDocumentHistory: api.history, comicMdExport: api.export, comicMdOptimize: api.optimize, comicMdRenderOptionsSave: api.options, comicMdSync: api.sync, comicMdWorkVisualSave: api.visualSave, comicMdWorkVisualExtract: api.visualExtract }));
vi.mock("./NovelAssetManager", () => ({ default: (props: { open: boolean; mode: string }) => { manager.props = props; return props.open ? <div role="dialog" aria-label="共享小说原文管理器">{props.mode}</div> : null; } }));
import MarkdownComicWorkspace from "./MarkdownComicWorkspace";
import { useLibraryStore } from "../../store/useLibraryStore";
let data: MdWorkspace;
beforeEach(() => {
  vi.clearAllMocks(); localStorage.clear();
  useLibraryStore.getState().loadAssets([]);
  data = { sourceRevisionId: "r1", sourceContent: "原著正文", documents: [], jobs: [], images: [], textReady: false, imageReady: false };
  api.list.mockResolvedValue([{ id: "w", projectId: "p", title: "测试小说", status: "active" }]);
  api.get.mockResolvedValue({ chapters: [{ id: "c1", chapterNo: 1, title: "第一章" }, { id: "c2", chapterNo: 2, title: "第二章" }] });
  api.workspace.mockImplementation(async () => structuredClone(data));
  api.save.mockImplementation(async (input) => {
    const doc = { id: "d", kind: input.kind, pageNo: input.pageNo ?? null, markdown: input.markdown, optimizationInstruction: input.optimizationInstruction ?? "", revision: (input.expectedRevision ?? 0) + 1, stale: false, issues: [], updatedAt: 1 };
    data.documents = [...data.documents.filter((item) => !(item.kind === doc.kind && item.pageNo === doc.pageNo)), doc]; return doc;
  });
  api.history.mockResolvedValue([{ revision: 1, markdown: "旧版完整文字", createdAt: 1 }]); api.export.mockResolvedValue({ path: "D:/export", files: ["page.md"] }); api.render.mockResolvedValue({ id: "j", status: "running" });
  api.sync.mockResolvedValue({ id: "sync", kind: "sync", status: "running" });
  api.optimize.mockResolvedValue({ id: "opt", kind: "optimize", status: "running" });
  api.options.mockImplementation(async (input) => { data.renderOptions = { promptInjection: input.promptInjection, revision: input.expectedRevision + 1 }; return data.renderOptions; });
  api.visualSave.mockImplementation(async (input) => {
    data.workVisualProfile = { constitutionMarkdown: input.constitutionMarkdown, revision: input.expectedRevision + 1, references: input.references.map((reference: { assetId: string; role: string; weight: number; sortOrder: number }) => ({ ...reference, fileAvailable: true })) };
    return data.workVisualProfile;
  });
  api.visualExtract.mockResolvedValue({ constitutionMarkdown: "## 总体风格\n黑白水墨", profileRevision: 1, referenceAssetIds: ["style-image"] });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });
  const open = async () => { render(<MarkdownComicWorkspace />); await screen.findByLabelText("小说原文预览"); };
describe("Markdown comic workflow", () => {
  it("submits the visible linked-update plan, reacts immediately to drafts, and never starts images", async () => {
    data.documents = [{ id: "s", kind: "settings", pageNo: null, markdown: "设定", revision: 2, stale: true, staleReasons: ["正文人物外貌发生变化"], issues: [], updatedAt: 1 }];
    data.syncPlan = { fingerprint: "current-plan", targets: [{ documentId: "s", revision: 2, kind: "settings", pageNo: null, reasons: ["正文人物外貌发生变化"] }], missingPageNos: [2], obsoletePageNos: [], blockedReason: null };
    await open(); expect(screen.getByLabelText("需要联动更新").textContent).toContain("1 份文字 + 1 页待补");
    fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    expect(screen.getAllByText("正文人物外貌发生变化").length).toBeGreaterThan(0);
    fireEvent.change(screen.getByLabelText("作品设定 Markdown"), { target: { value: "未保存的新设定" } });
    const sync = screen.getByRole("button", { name: "更新本章受影响文字" }) as HTMLButtonElement;
    expect(sync.disabled).toBe(true); fireEvent.click(sync); expect(api.sync).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("作品设定 Markdown"), { target: { value: "设定" } });
    expect(sync.disabled).toBe(false); fireEvent.click(sync);
    await waitFor(() => expect(api.sync).toHaveBeenCalledWith({ projectId: "p", novelWorkId: "w", chapterId: "c1", expectedPlanFingerprint: "current-plan" }));
    expect(api.generate).not.toHaveBeenCalled(); expect(api.render).not.toHaveBeenCalled();
  });
  it("saves same-work image references as the work visual profile before rendering pages", async () => {
    data.documents = [{ id: "page-1", kind: "page_prompt", pageNo: 1, markdown: "# 第1页\n\n完整 Prompt", revision: 1, stale: false, issues: [], updatedAt: 1 }];
    localStorage.setItem("comic-md:style-references:p:w", JSON.stringify([{ assetId: "style-image", path: "D:/style-image.png", label: "水墨参考", description: "黑白水墨，干笔线条" }]));
    useLibraryStore.getState().loadAssets([{ asset: { id: "style-image", kind: "image", path: "D:/style-image.png" }, source: "水墨参考", projectId: "p", createdAt: 1 }]);
    await open();
    fireEvent.click(screen.getByRole("button", { name: "6. 漫画" }));
    await screen.findByRole("button", { name: /生成第一页/ });
    fireEvent.click(screen.getByRole("button", { name: /生成第一页/ }));
    await waitFor(() => expect(api.visualSave).toHaveBeenCalledWith(expect.objectContaining({
      projectId: "p", novelWorkId: "w", references: [{ assetId: "style-image", role: "style", weight: 0.7, sortOrder: 0, note: "黑白水墨，干笔线条" }], expectedRevision: 0,
    })));
    expect(api.render).toHaveBeenCalledWith({ projectId: "p", novelWorkId: "w", chapterId: "c1", pages: [{ documentId: "page-1", revision: 1 }], expectedRenderOptionsRevision: 0 });
    expect(api.visualSave.mock.invocationCallOrder[0]).toBeLessThan(api.render.mock.invocationCallOrder[0]);
  });
  it("keeps the database visual profile authoritative while the library is still empty", async () => {
    data.workVisualProfile = { constitutionMarkdown: "## 总体风格\n水墨", revision: 3, references: [{ assetId: "saved-style", path: "D:/saved-style.png", role: "style", weight: 0.7, sortOrder: 0, note: "只参考线条", sha256: "sha256:test", fileAvailable: true }] };
    await open();
    fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    fireEvent.click(screen.getByRole("button", { name: "生成作品设定（仅文字）" }));
    await waitFor(() => expect(api.generate).toHaveBeenCalled());
    expect(api.visualSave).not.toHaveBeenCalled();
  });
  it("saves a changed work visual profile before AI optimization and stops when that save fails", async () => {
    data.documents = [{ id: "settings", kind: "settings", pageNo: null, markdown: "## 世界观\n城市\n## 画风\n旧画风\n## 人物锚点\n甲", revision: 1, stale: false, issues: [], updatedAt: 1 }];
    localStorage.setItem("comic-md:style-references:p:w", JSON.stringify([{ assetId: "style-image", path: "D:/style-image.png", label: "新画风" }]));
    useLibraryStore.getState().loadAssets([{ asset: { id: "style-image", kind: "image", path: "D:/style-image.png" }, source: "新画风", projectId: "p", createdAt: 1 }]);
    api.visualSave.mockRejectedValueOnce(new Error("作品视觉宪法已有新版本"));
    await open();
    fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    fireEvent.change(screen.getByLabelText("AI 优化要求"), { target: { value: "按新画风优化" } });
    fireEvent.click(screen.getByRole("button", { name: "AI 优化" }));
    await waitFor(() => expect(api.visualSave).toHaveBeenCalled());
    expect(api.optimize).not.toHaveBeenCalled();
    expect(await screen.findByText(/服务器中的作品视觉设定已有新版本/)).toBeTruthy();
  });
  it("navigates an affected chapter by ID and preserves its independent comic draft", async () => {
    data.affectedChapters = [{ chapterId: "c2", chapterNo: 9, title: "远方来信", documentCount: 3, reason: "人物锚点变化" }];
    data.documents = [{ id: "settings", kind: "settings", pageNo: null, markdown: "原作品设定", revision: 1, stale: false, issues: [], updatedAt: 1 }];
    api.get.mockResolvedValue({ chapters: [{ id: "c1", chapterNo: 1, title: "第一章" }, { id: "c2", chapterNo: 9, title: "远方来信" }] });
    api.workspace.mockImplementation(async ({ chapterId }) => ({ ...structuredClone(data), sourceContent: chapterId === "c2" ? "第九章正文" : "原著正文" }));
    await open(); fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" })); fireEvent.change(screen.getByLabelText("作品设定 Markdown"), { target: { value: "第一章未保存的漫画改编稿" } });
    fireEvent.click(screen.getByRole("button", { name: "前往第9章处理" }));
    await waitFor(() => expect((screen.getByLabelText("小说原文预览") as HTMLTextAreaElement).value).toBe("第九章正文"));
    expect((screen.getByLabelText("选择章节") as HTMLSelectElement).value).toBe("c2");
    expect(api.workspace).toHaveBeenLastCalledWith({ projectId: "p", novelWorkId: "w", chapterId: "c2" });
    fireEvent.change(screen.getByLabelText("选择章节"), { target: { value: "c1" } });
    await screen.findByLabelText("小说原文预览"); fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    expect((screen.getByLabelText("作品设定 Markdown") as HTMLTextAreaElement).value).toBe("第一章未保存的漫画改编稿");
  });
  it("blocks missing-page drafts and active jobs before linked updates", async () => {
    let poll!: () => void;
    vi.spyOn(window, "setInterval").mockImplementation((handler) => { poll = handler as () => void; return 123; });
    data.syncPlan = { fingerprint: "plan", targets: [], missingPageNos: [2], obsoletePageNos: [], blockedReason: null };
    localStorage.setItem("comic-md:draft:p:w:c1:page_prompt:2", JSON.stringify({ markdown: "未保存第2页", expectedRevision: null }));
    await open();
    expect((screen.getByRole("button", { name: "更新本章受影响文字" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByRole("alert").textContent).toContain("待补的第2页有未保存草稿");
    localStorage.removeItem("comic-md:draft:p:w:c1:page_prompt:2");
    data.jobs = [{ id: "live", kind: "sync", status: "running", message: "正在更新第2页", outputMarkdown: null, completedPages: 0, totalPages: 1, createdAt: 1 }];
    await act(async () => poll());
    fireEvent.click(screen.getByRole("button", { name: "更新本章受影响文字" }));
    expect(api.sync).not.toHaveBeenCalled();
    expect(screen.getByLabelText("当前生成状态").textContent).toContain("0/1 份");
  });
  it("does not offer metadata-only revised pages as remaining images", async () => {
    data.documents = [{ id: "d1", kind: "page_prompt", pageNo: 1, markdown: "已画正文", contentHash: "body-hash", optimizationInstruction: "新优化要求", revision: 4, stale: false, issues: [], updatedAt: 1 }];
    data.images = [{ id: "i1", documentId: "d1", documentRevision: 2, contentHash: "body-hash", pageNo: 1, path: "img", promptInjection: "", stale: false, createdAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "6. 漫画" }));
    const remaining = screen.getByRole("button", { name: "生成剩余页" }) as HTMLButtonElement;
    expect(remaining.disabled).toBe(true); fireEvent.click(remaining); expect(api.render).not.toHaveBeenCalled();
    expect((screen.getByRole("button", { name: "生成第一页（1 页）" }) as HTMLButtonElement).disabled).toBe(false);
  });
  it("does not acknowledge stale dependencies when saving only instructions or optimizing", async () => {
    data.documents = [{ id: "s", kind: "settings", pageNo: null, markdown: "原设定", optimizationInstruction: "原要求", revision: 2, stale: true, issues: [], updatedAt: 1 }];
    api.save.mockImplementation(async (input) => { const doc = { ...data.documents[0], markdown: input.markdown, optimizationInstruction: input.optimizationInstruction, revision: input.expectedRevision + 1, stale: !input.acknowledgeUpdates }; data.documents = [doc]; return doc; });
    await open(); fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    fireEvent.change(screen.getByLabelText("AI 优化要求"), { target: { value: "只改要求" } });
    fireEvent.click(screen.getByRole("button", { name: "保存文字" }));
    await screen.findByRole("button", { name: "保存更新" });
    expect(api.save.mock.calls[0][0].acknowledgeUpdates).not.toBe(true);
    fireEvent.change(screen.getByLabelText("AI 优化要求"), { target: { value: "优化动作" } });
    fireEvent.click(screen.getByRole("button", { name: "AI 优化" }));
    await waitFor(() => expect(api.optimize).toHaveBeenCalled());
    expect(api.save.mock.calls.every(([input]) => !input.acknowledgeUpdates)).toBe(true);
    await screen.findByRole("button", { name: "保存更新" });
    fireEvent.click(screen.getByRole("button", { name: "保存更新" }));
    await waitFor(() => expect(api.save).toHaveBeenLastCalledWith(expect.objectContaining({ acknowledgeUpdates: true, markdown: "原设定", optimizationInstruction: "优化动作" })));
  });
  it("keeps obsolete prompts accessible but excludes them from rendering, default export and all-page optimization", async () => {
    data.documents = [1, 2].map((pageNo) => ({ id: `d${pageNo}`, kind: "page_prompt" as const, pageNo, markdown: `完整第${pageNo}页`, optimizationInstruction: "改善动作", revision: 2, stale: false, outOfPlan: pageNo === 2, issues: [], updatedAt: 1 }));
    await open(); fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    fireEvent.click(screen.getByRole("button", { name: /第2页.*不在当前分镜/ }));
    expect((screen.getByLabelText("本页 Prompt Markdown") as HTMLTextAreaElement).value).toBe("完整第2页");
    expect((screen.getByRole("button", { name: "生成这一页漫画" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByLabelText("优化全部页") as HTMLInputElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "导出旧页 MD" }));
    await waitFor(() => expect(api.export).toHaveBeenLastCalledWith(expect.objectContaining({ documentIds: ["d2"] })));
    fireEvent.click(screen.getByRole("button", { name: "6. 漫画" }));
    fireEvent.click(screen.getByRole("button", { name: "生成前三页（1 页）" }));
    await waitFor(() => expect(api.render).toHaveBeenCalledWith(expect.objectContaining({ pages: [{ documentId: "d1", revision: 2 }] })));
    await waitFor(() => expect((screen.getByRole("button", { name: "导出本章全部已保存 MD" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "导出本章全部已保存 MD" }));
    await waitFor(() => expect(api.export).toHaveBeenLastCalledWith(expect.objectContaining({ documentIds: ["d1"] })));
    fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    fireEvent.click(screen.getByRole("button", { name: /^第1页$/ }));
    fireEvent.click(screen.getByLabelText("优化全部页"));
    fireEvent.click(screen.getByRole("button", { name: "AI 优化" }));
    await waitFor(() => expect(api.optimize).toHaveBeenCalledWith(expect.objectContaining({ targets: [{ documentId: "d1", revision: 2 }] })));
  });
  it("saves Markdown and optimization instruction before submitting the saved revision for AI optimization", async () => {
    data.documents = [{ id: "d", kind: "page_prompt", pageNo: 1, markdown: "旧Prompt", optimizationInstruction: "旧要求", revision: 4, stale: false, issues: [], updatedAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    fireEvent.change(screen.getByLabelText("本页 Prompt Markdown"), { target: { value: "新Prompt草稿" } });
    fireEvent.change(screen.getByLabelText("AI 优化要求"), { target: { value: "加强人物动作" } });
    fireEvent.click(screen.getByRole("button", { name: "AI 优化" }));
    await waitFor(() => expect(api.optimize).toHaveBeenCalledWith({ projectId: "p", novelWorkId: "w", chapterId: "c1", targets: [{ documentId: "d", revision: 5 }], instruction: "加强人物动作" }));
    expect(api.save).toHaveBeenCalledWith(expect.objectContaining({ markdown: "新Prompt草稿", optimizationInstruction: "加强人物动作", expectedRevision: 4 }));
    expect(api.save.mock.invocationCallOrder[0]).toBeLessThan(api.optimize.mock.invocationCallOrder[0]);
    expect(api.render).not.toHaveBeenCalled();
  });
  it("preserves current Markdown and instruction if saving before optimization fails", async () => {
    api.save.mockRejectedValueOnce(new Error("版本冲突"));
    await open(); fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    fireEvent.change(screen.getByLabelText("作品设定 Markdown"), { target: { value: "还没保存的设定" } });
    fireEvent.change(screen.getByLabelText("AI 优化要求"), { target: { value: "补充世界观" } });
    fireEvent.click(screen.getByRole("button", { name: "AI 优化" }));
    await screen.findByText(/版本冲突/);
    expect((screen.getByLabelText("作品设定 Markdown") as HTMLTextAreaElement).value).toBe("还没保存的设定");
    expect((screen.getByLabelText("AI 优化要求") as HTMLTextAreaElement).value).toBe("补充世界观");
    expect(api.optimize).not.toHaveBeenCalled();
  });
  it("switches real previous versions in the same editor and preserves the unsaved current text and instruction", async () => {
    data.documents = [{ id: "d", kind: "script", pageNo: null, markdown: "当前剧本", optimizationInstruction: "当前要求", revision: 3, stale: false, issues: [], updatedAt: 3 }];
    api.history.mockResolvedValue([{ revision: 3, markdown: "当前剧本", optimizationInstruction: "当前要求", createdAt: 3 }, { revision: 2, markdown: "历史剧本", optimizationInstruction: "历史要求", createdAt: 2 }]);
    await open(); fireEvent.click(screen.getByRole("button", { name: "3. 本章剧本" }));
    await screen.findByRole("option", { name: "第 2 版" });
    fireEvent.change(screen.getByLabelText("本章剧本 Markdown"), { target: { value: "未保存当前草稿" } });
    fireEvent.change(screen.getByLabelText("AI 优化要求"), { target: { value: "未保存当前要求" } });
    fireEvent.click(screen.getByRole("button", { name: "上一版" }));
    expect((screen.getByLabelText("本章剧本 Markdown") as HTMLTextAreaElement).value).toBe("历史剧本");
    expect((screen.getByLabelText("AI 优化要求") as HTMLTextAreaElement).value).toBe("历史要求");
    fireEvent.click(screen.getByRole("button", { name: "采用此版本并保存" }));
    await waitFor(() => expect(api.save).toHaveBeenCalledWith(expect.objectContaining({ markdown: "历史剧本", optimizationInstruction: "历史要求", expectedRevision: 3 })));
    await waitFor(() => expect((screen.getByRole("button", { name: "返回当前草稿" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "返回当前草稿" }));
    expect((screen.getByLabelText("本章剧本 Markdown") as HTMLTextAreaElement).value).toBe("未保存当前草稿");
    expect((screen.getByLabelText("AI 优化要求") as HTMLTextAreaElement).value).toBe("未保存当前要求");
  });
  it("allows export and rendering when selecting the already-current head without a live draft", async () => {
    data.documents = [{ id: "d", kind: "page_prompt", pageNo: 1, markdown: "当前Prompt", optimizationInstruction: "要求", revision: 3, stale: false, issues: [], updatedAt: 3 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    fireEvent.change(screen.getByLabelText("文档版本"), { target: { value: "3" } });
    expect((screen.getByRole("button", { name: "导出 MD" }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByRole("button", { name: "生成这一页漫画" }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByRole("button", { name: "采用此版本并保存" }) as HTMLButtonElement).disabled).toBe(true);
  });
  it("shows the new optimized version after optimizing history while retaining the parked current draft", async () => {
    data.documents = [{ id: "d", kind: "script", pageNo: null, markdown: "当前剧本", optimizationInstruction: "当前要求", revision: 3, stale: false, issues: [], updatedAt: 3 }];
    api.history.mockResolvedValue([{ revision: 2, markdown: "历史剧本", optimizationInstruction: "改好历史对白", createdAt: 2 }]);
    api.optimize.mockImplementation(async () => {
      data.documents[0] = { ...data.documents[0], markdown: "AI优化后的新剧本", optimizationInstruction: "改好历史对白", revision: 5 };
      const job = { id: "opt", kind: "optimize" as const, status: "succeeded" as const, message: null, outputMarkdown: null, completedPages: 1, totalPages: 1, createdAt: 4 };
      data.jobs = [job]; return job;
    });
    await open(); fireEvent.click(screen.getByRole("button", { name: "3. 本章剧本" }));
    fireEvent.change(screen.getByLabelText("本章剧本 Markdown"), { target: { value: "保留我的当前草稿" } });
    await screen.findByRole("option", { name: "第 2 版" });
    fireEvent.click(screen.getByRole("button", { name: "上一版" }));
    fireEvent.click(screen.getByRole("button", { name: "AI 优化" }));
    await waitFor(() => expect((screen.getByLabelText("本章剧本 Markdown") as HTMLTextAreaElement).value).toBe("AI优化后的新剧本"));
    expect((screen.getByLabelText("AI 优化要求") as HTMLTextAreaElement).value).toBe("改好历史对白");
    fireEvent.click(screen.getByRole("button", { name: "返回当前草稿" }));
    expect((screen.getByLabelText("本章剧本 Markdown") as HTMLTextAreaElement).value).toBe("保留我的当前草稿");
  });
  it("does not switch away from text entered while the optimization request is being submitted", async () => {
    let finish!: () => void;
    data.documents = [{ id: "d", kind: "script", pageNo: null, markdown: "已保存剧本", optimizationInstruction: "要求", revision: 3, stale: false, issues: [], updatedAt: 3 }];
    api.optimize.mockImplementation(() => new Promise((resolve) => { finish = () => {
      data.documents[0] = { ...data.documents[0], markdown: "AI结果", revision: 4 };
      const job = { id: "opt", kind: "optimize" as const, status: "succeeded" as const, message: null, outputMarkdown: null, completedPages: 1, totalPages: 1, createdAt: 4 }; data.jobs = [job]; resolve(job);
    }; }));
    await open(); fireEvent.click(screen.getByRole("button", { name: "3. 本章剧本" }));
    fireEvent.click(screen.getByRole("button", { name: "AI 优化" }));
    await waitFor(() => expect(api.optimize).toHaveBeenCalled());
    fireEvent.change(screen.getByLabelText("本章剧本 Markdown"), { target: { value: "网络等待时新写内容" } });
    fireEvent.change(screen.getByLabelText("AI 优化要求"), { target: { value: "网络等待时新要求" } });
    await act(async () => finish());
    expect((screen.getByLabelText("本章剧本 Markdown") as HTMLTextAreaElement).value).toBe("网络等待时新写内容");
    expect((screen.getByLabelText("AI 优化要求") as HTMLTextAreaElement).value).toBe("网络等待时新要求");
    expect((screen.getByLabelText("文档版本") as HTMLSelectElement).value).toBe("current");
  });
  it("optimizes all saved pages in order with one instruction and the newly saved current revision", async () => {
    data.documents = [1, 2, 3].map((no) => ({ id: no === 1 ? "d" : `d${no}`, kind: "page_prompt" as const, pageNo: no, markdown: `第${no}页Prompt`, revision: no, stale: false, issues: [], updatedAt: 1 }));
    await open(); fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    expect((screen.getByRole("checkbox", { name: "优化全部页" }) as HTMLInputElement).checked).toBe(false);
    fireEvent.change(screen.getByLabelText("AI 优化要求"), { target: { value: "统一画风" } });
    fireEvent.click(screen.getByRole("checkbox", { name: "优化全部页" }));
    fireEvent.click(screen.getByRole("button", { name: "AI 优化" }));
    await waitFor(() => expect(api.optimize).toHaveBeenCalledWith(expect.objectContaining({ targets: [{ documentId: "d", revision: 2 }, { documentId: "d2", revision: 2 }, { documentId: "d3", revision: 3 }], instruction: "统一画风", allPages: true })));
  });
  it("does not call optimize-all while a storyboard page prompt is missing", async () => {
    data.documents = [1, 3].map((no) => ({ id: `d${no}`, kind: "page_prompt" as const, pageNo: no, markdown: `第${no}页Prompt`, revision: 1, stale: false, issues: [], updatedAt: 1 }));
    data.syncPlan = { fingerprint: "p", targets: [], missingPageNos: [2], obsoletePageNos: [], blockedReason: null };
    await open(); fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    fireEvent.change(screen.getByLabelText("AI 优化要求"), { target: { value: "统一画风" } });
    fireEvent.click(screen.getByRole("checkbox", { name: "优化全部页" }));
    expect((screen.getByRole("button", { name: "AI 优化" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText(/分镜仍缺少第2页 Prompt/)).toBeTruthy();
    expect(api.optimize).not.toHaveBeenCalled();
  });
  it("does not ignore another page's unsaved optimization instruction in optimize-all", async () => {
    data.documents = [1, 2].map((no) => ({ id: `d${no}`, kind: "page_prompt" as const, pageNo: no, markdown: `第${no}页Prompt`, revision: 1, stale: false, issues: [], updatedAt: 1 }));
    localStorage.setItem("comic-md:draft:p:w:c1:page_prompt:2", JSON.stringify({ markdown: "第2页Prompt", optimizationInstruction: "别页新要求", expectedRevision: 1 }));
    await open(); fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    fireEvent.change(screen.getByLabelText("AI 优化要求"), { target: { value: "统一画风" } });
    fireEvent.click(screen.getByRole("checkbox", { name: "优化全部页" }));
    expect((screen.getByRole("button", { name: "AI 优化" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText(/第2页有未保存修改或版本冲突/)).toBeTruthy();
    expect(api.optimize).not.toHaveBeenCalled();
  });
  it("saves pending chapter injection before rendering from the page editor and passes the returned revision", async () => {
    data.documents = [{ id: "d", kind: "page_prompt", pageNo: 1, markdown: "Prompt", revision: 2, stale: false, issues: [], updatedAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "6. 漫画" }));
    fireEvent.change(screen.getByLabelText("本章 Prompt 注入"), { target: { value: "黑白漫画，简体中文" } });
    fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    fireEvent.click(screen.getByRole("button", { name: "生成这一页漫画" }));
    await waitFor(() => expect(api.render).toHaveBeenCalledWith({ projectId: "p", novelWorkId: "w", chapterId: "c1", pages: [{ documentId: "d", revision: 2 }], expectedRenderOptionsRevision: 1 }));
    expect(api.options).toHaveBeenCalledWith({ projectId: "p", novelWorkId: "w", chapterId: "c1", promptInjection: "黑白漫画，简体中文", expectedRevision: 0 });
    expect(api.options.mock.invocationCallOrder[0]).toBeLessThan(api.render.mock.invocationCallOrder[0]);
  });
  it("keeps injection draft and avoids rendering when injection saving fails", async () => {
    data.documents = [{ id: "d", kind: "page_prompt", pageNo: 1, markdown: "Prompt", revision: 2, stale: false, issues: [], updatedAt: 1 }];
    api.options.mockRejectedValueOnce(new Error("注入版本冲突"));
    await open(); fireEvent.click(screen.getByRole("button", { name: "6. 漫画" }));
    fireEvent.change(screen.getByLabelText("本章 Prompt 注入"), { target: { value: "我的注入" } });
    fireEvent.click(screen.getByRole("button", { name: "生成第一页（1 页）" }));
    await screen.findByText(/注入版本冲突/);
    expect((screen.getByLabelText("本章 Prompt 注入") as HTMLTextAreaElement).value).toBe("我的注入");
    expect(api.render).not.toHaveBeenCalled();
  });
  it("uses and preserves an optional page-scoped injection only for rerunning that image", async () => {
    data.documents = [{ id: "d1", kind: "page_prompt", pageNo: 1, markdown: "Prompt", revision: 2, stale: false, issues: [], updatedAt: 1 }];
    data.renderOptions = { promptInjection: "本章统一黑白", revision: 4 };
    data.images = [{ id: "i1", documentId: "d1", documentRevision: 2, pageNo: 1, path: "img", promptInjection: "本章统一黑白", rerunPromptInjection: "旧的单图修订", stale: false, createdAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "6. 漫画" }));
    expect(screen.getByText(/本次重画注入：/).parentElement?.textContent).toContain("旧的单图修订");
    const input = screen.getByLabelText("第1页本次重画 Prompt 注入") as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: "只把雨伞改为红色" } });
    fireEvent.click(screen.getByRole("button", { name: "重画第1页" }));
    await waitFor(() => expect(api.render).toHaveBeenLastCalledWith({ projectId: "p", novelWorkId: "w", chapterId: "c1", pages: [{ documentId: "d1", revision: 2 }], expectedRenderOptionsRevision: 4, rerunPromptInjection: "只把雨伞改为红色" }));
    expect((screen.getByLabelText("第1页本次重画 Prompt 注入") as HTMLTextAreaElement).value).toBe("只把雨伞改为红色");
    expect(JSON.parse(localStorage.getItem("comic-md:rerun-injections:p:w:c1")!)).toEqual({ 1: "只把雨伞改为红色" });
  });
  it("submits exactly first, first three and remaining pages from their explicit buttons", async () => {
    data.documents = [1, 2, 3, 4].map((pageNo) => ({ id: `d${pageNo}`, kind: "page_prompt" as const, pageNo, markdown: "Prompt", revision: 2, stale: false, issues: [], updatedAt: 1 }));
    data.renderOptions = { promptInjection: "统一规则", revision: 7 };
    data.images = [{ id: "i", documentId: "d1", documentRevision: 2, pageNo: 1, path: "img", promptInjection: "统一规则", stale: false, createdAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "6. 漫画" }));
    const expected = [["生成第一页（1 页）", ["d1"]], ["生成前三页（3 页）", ["d1", "d2", "d3"]], ["生成剩余页（3 页）", ["d2", "d3", "d4"]]] as const;
    for (const [name, ids] of expected) {
      await waitFor(() => expect((screen.getByRole("button", { name }) as HTMLButtonElement).disabled).toBe(false));
      fireEvent.click(screen.getByRole("button", { name }));
      await waitFor(() => expect(api.render).toHaveBeenLastCalledWith({ projectId: "p", novelWorkId: "w", chapterId: "c1", pages: ids.map((documentId) => ({ documentId, revision: 2 })), expectedRenderOptionsRevision: 7 }));
    }
    expect(api.options).not.toHaveBeenCalled();
  });
  it("uses the shared manager in select mode and enters the explicitly selected new chapter", async () => {
    await open(); fireEvent.click(screen.getByRole("button", { name: "选择 / 新增章节" }));
    await screen.findByRole("dialog", { name: "共享小说原文管理器" });
    const props = manager.props as { mode: string; initialSelection?: { projectId: string; novelWorkId: string; novelChapterId: string }; onSelect: (selection: { projectId: string; novelWorkId: string; workTitle: string; novelChapterId: string; novelChapterRevisionId: string; revisionNo: number; chapterNo: number; title: string; content: string }) => void };
    expect(props.mode).toBe("select");
    expect(props.initialSelection).toEqual({ projectId: "p", novelWorkId: "w", novelChapterId: "c1" });
    act(() => props.onSelect({ projectId: "p", novelWorkId: "w", workTitle: "测试小说", novelChapterId: "c2", novelChapterRevisionId: "r2", revisionNo: 1, chapterNo: 2, title: "第二章", content: "第二章原文" }));
    await waitFor(() => expect((screen.getByLabelText("选择章节") as HTMLSelectElement).value).toBe("c2"));
    await waitFor(() => expect((screen.getByLabelText("小说原文预览") as HTMLTextAreaElement).value).toBe("原著正文"));
    expect(api.workspace).toHaveBeenLastCalledWith({ projectId: "p", novelWorkId: "w", chapterId: "c2" });
  });
  it("refreshes a changed source into the comic stale workflow without overwriting saved comic documents", async () => {
    data.documents = [{ id: "settings", kind: "settings", pageNo: null, markdown: "保留的漫画作品设定", revision: 2, stale: false, issues: [], updatedAt: 1 }];
    await open();
    const beforeRefresh = api.workspace.mock.calls.length;
    data = { ...data, sourceRevisionId: "r2", sourceContent: "更新后的小说原文", documents: [{ ...data.documents[0], stale: true, staleReasons: ["小说原文有新修订"] }] };
    const props = manager.props as { onChanged: (change: { projectId: string; novelWorkId: string; novelChapterId: string; novelChapterRevisionId: string }) => void };
    act(() => props.onChanged({ projectId: "p", novelWorkId: "w", novelChapterId: "c1", novelChapterRevisionId: "r2" }));
    await waitFor(() => expect(api.workspace.mock.calls.length).toBeGreaterThan(beforeRefresh));
    await waitFor(() => expect((screen.getByLabelText("小说原文预览") as HTMLTextAreaElement).value).toBe("更新后的小说原文"));
    fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    expect((screen.getByLabelText("作品设定 Markdown") as HTMLTextAreaElement).value).toBe("保留的漫画作品设定");
    expect(api.save).not.toHaveBeenCalled(); expect(api.generate).not.toHaveBeenCalled(); expect(api.render).not.toHaveBeenCalled();
  });
  it("does not create a comic revision for unchanged Markdown", async () => {
    data.documents = [{ id: "s", kind: "settings", pageNo: null, markdown: "原设定", revision: 1, stale: false, issues: [], updatedAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    const mdSave = screen.getByRole("button", { name: "保存文字" }) as HTMLButtonElement;
    expect(mdSave.disabled).toBe(true); fireEvent.click(mdSave); expect(api.save).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("作品设定 Markdown"), { target: { value: "临时修改" } });
    fireEvent.change(screen.getByLabelText("作品设定 Markdown"), { target: { value: "原设定" } });
    expect(mdSave.disabled).toBe(true);
    expect((screen.getByRole("button", { name: "导出 MD" }) as HTMLButtonElement).disabled).toBe(false);
  });
  it("allows confirming an unchanged stale document as updated", async () => {
    data.documents = [{ id: "s", kind: "settings", pageNo: null, markdown: "已核对的设定", revision: 1, stale: true, issues: [], updatedAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    const save = screen.getByRole("button", { name: "保存更新" }) as HTMLButtonElement;
    expect(save.disabled).toBe(false); fireEvent.click(save);
    await waitFor(() => expect(api.save).toHaveBeenCalledWith(expect.objectContaining({ markdown: "已核对的设定", expectedRevision: 1, acknowledgeUpdates: true })));
  });
  it("reverting a prompt edit restores export and single/batch rendering without adding a revision", async () => {
    data.documents = [{ id: "d", kind: "page_prompt", pageNo: 1, markdown: "已保存Prompt", revision: 2, stale: false, issues: [], updatedAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    fireEvent.change(screen.getByLabelText("本页 Prompt Markdown"), { target: { value: "临时修改" } });
    fireEvent.change(screen.getByLabelText("本页 Prompt Markdown"), { target: { value: "已保存Prompt" } });
    expect((screen.getByRole("button", { name: "保存文字" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "导出 MD" }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByRole("button", { name: "生成这一页漫画" }) as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "6. 漫画" }));
    expect((screen.getByRole("button", { name: "生成剩余页（1 页）" }) as HTMLButtonElement).disabled).toBe(false);
    expect(api.save).not.toHaveBeenCalled();
  });
  it("keeps an older draft baseline conflicting even when its text matches the current saved document", async () => {
    data.documents = [{ id: "d", kind: "page_prompt", pageNo: 1, markdown: "当前Prompt", revision: 2, stale: false, issues: [], updatedAt: 1 }];
    localStorage.setItem("comic-md:draft:p:w:c1:page_prompt:1", JSON.stringify({ markdown: "旧稿", expectedRevision: 1 }));
    await open(); fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    fireEvent.change(screen.getByLabelText("本页 Prompt Markdown"), { target: { value: "当前Prompt" } });
    expect(JSON.parse(localStorage.getItem("comic-md:draft:p:w:c1:page_prompt:1")!).expectedRevision).toBe(1);
    expect((screen.getByRole("button", { name: "生成这一页漫画" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "6. 漫画" }));
    expect((screen.getByRole("button", { name: "生成剩余页" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    fireEvent.click(screen.getByRole("button", { name: "保留草稿，以当前版本为基准" }));
    expect((screen.getByRole("button", { name: "生成这一页漫画" }) as HTMLButtonElement).disabled).toBe(false);
    expect(api.save).not.toHaveBeenCalled();
  });
  it("treats a restored draft equal to the saved text and revision as clean for export and all rendering", async () => {
    data.documents = [{ id: "d", kind: "page_prompt", pageNo: 1, markdown: "相同Prompt", revision: 2, stale: false, issues: [], updatedAt: 1 }];
    localStorage.setItem("comic-md:draft:p:w:c1:page_prompt:1", JSON.stringify({ markdown: "相同Prompt", expectedRevision: 2 }));
    await open(); fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    expect((screen.getByRole("button", { name: "保存文字" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "导出 MD" }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByRole("button", { name: "生成这一页漫画" }) as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "6. 漫画" }));
    expect((screen.getByRole("button", { name: "生成剩余页（1 页）" }) as HTMLButtonElement).disabled).toBe(false);
    expect(api.save).not.toHaveBeenCalled();
  });
  it("accepts an independent page prompt, copies exact Markdown and renders only after saving", async () => {
    const user = userEvent.setup(); await open(); await user.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    expect((screen.getByRole("button", { name: "生成这一页漫画" }) as HTMLButtonElement).disabled).toBe(true);
    const markdown = "# 第1页\n\n## 人物锚点\n林青，黑发\n";
    fireEvent.change(screen.getByLabelText("本页 Prompt Markdown"), { target: { value: markdown } }); await user.click(screen.getByRole("button", { name: "复制全文" })); expect(await navigator.clipboard.readText()).toBe(markdown);
    await user.click(screen.getByRole("button", { name: "保存文字" })); await waitFor(() => expect((screen.getByRole("button", { name: "生成这一页漫画" }) as HTMLButtonElement).disabled).toBe(false));
    expect(api.generate).not.toHaveBeenCalled(); expect(api.render).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "生成这一页漫画" })); expect(api.render).toHaveBeenCalledWith({ projectId: "p", novelWorkId: "w", chapterId: "c1", pages: [{ documentId: "d", revision: 1 }], expectedRenderOptionsRevision: 0 });
  });
  it("retains dirty text and its old revision on chapter navigation", async () => {
    data.documents = [{ id: "s", kind: "settings", pageNo: null, markdown: "原设定", revision: 1, stale: false, issues: [], updatedAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" })); fireEvent.change(screen.getByLabelText("作品设定 Markdown"), { target: { value: "未保存的独立草稿" } });
    fireEvent.change(screen.getByLabelText("选择章节"), { target: { value: "c2" } }); await screen.findByLabelText("小说原文预览"); data.documents[0].revision = 2;
    fireEvent.change(screen.getByLabelText("选择章节"), { target: { value: "c1" } }); await screen.findByLabelText("小说原文预览"); fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    expect((screen.getByLabelText("作品设定 Markdown") as HTMLTextAreaElement).value).toBe("未保存的独立草稿"); fireEvent.click(screen.getByRole("button", { name: "保存文字" }));
    await waitFor(() => expect(api.save).toHaveBeenCalledWith(expect.objectContaining({ markdown: "未保存的独立草稿", expectedRevision: 1 })));
  });
  it("blocks stale prompts and keeps invalid model output accessible with history/export", async () => {
    data.documents = [{ id: "d", kind: "page_prompt", pageNo: 1, markdown: "# 第1页\n原Prompt", revision: 2, stale: true, issues: ["请补齐人物锚点"], updatedAt: 1 }];
    data.jobs = [{ id: "failed", kind: "page_prompts", status: "failed", message: "缺必要节点", outputMarkdown: "模型原始结果不能丢", completedPages: 0, totalPages: 0, createdAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" })); expect((screen.getByRole("button", { name: "生成这一页漫画" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByLabelText("生成原始 Markdown") as HTMLTextAreaElement).value).toBe("模型原始结果不能丢");
    fireEvent.click(screen.getByRole("button", { name: "历史版本" })); await screen.findByRole("option", { name: "第 1 版" }); fireEvent.change(screen.getByLabelText("文档版本"), { target: { value: "1" } }); expect((screen.getByLabelText("本页 Prompt Markdown") as HTMLTextAreaElement).value).toBe("旧版完整文字"); fireEvent.click(screen.getByRole("button", { name: "返回当前草稿" }));
    fireEvent.click(screen.getByRole("button", { name: "导出 MD" })); await waitFor(() => expect(api.export).toHaveBeenCalledWith({ projectId: "p", novelWorkId: "w", chapterId: "c1", documentIds: ["d"] }));
  });
  it("does not let a late chapter response replace the selected chapter", async () => {
    let resolveOld!: (workspace: MdWorkspace) => void;
    api.workspace.mockImplementation(({ chapterId }) => chapterId === "c1" ? new Promise<MdWorkspace>((resolve) => { resolveOld = resolve; }) : Promise.resolve({ ...data, sourceContent: "第二章内容" }));
    render(<MarkdownComicWorkspace />); await screen.findByLabelText("选择章节"); await waitFor(() => expect(api.workspace).toHaveBeenCalled()); fireEvent.change(screen.getByLabelText("选择章节"), { target: { value: "c2" } });
    await screen.findByLabelText("小说原文预览"); resolveOld({ ...data, sourceContent: "第一章迟到结果" }); await waitFor(() => expect((screen.getByLabelText("小说原文预览") as HTMLTextAreaElement).value).toBe("第二章内容"));
  });
  it("polls persisted jobs without overwriting dirty Markdown and blocks duplicate requests", async () => {
    let poll!: () => void;
    vi.spyOn(window, "setInterval").mockImplementation((handler) => { poll = handler as () => void; return 123; });
    data.documents = [{ id: "s", kind: "settings", pageNo: null, markdown: "原设定", revision: 1, stale: false, issues: [], updatedAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    fireEvent.change(screen.getByLabelText("作品设定 Markdown"), { target: { value: "用户正在编辑" } });
    data.documents[0].markdown = "后台新生成"; data.documents[0].revision = 2;
    data.jobs = [{ id: "live", kind: "settings", status: "running", message: null, outputMarkdown: null, completedPages: 0, totalPages: 0, createdAt: 1 }];
    await act(async () => poll());
    expect((screen.getByLabelText("作品设定 Markdown") as HTMLTextAreaElement).value).toBe("用户正在编辑");
    expect((screen.getByRole("button", { name: "任务进行中…" }) as HTMLButtonElement).disabled).toBe(true);
    expect(api.generate).not.toHaveBeenCalled();
  });
  it("late save cannot delete a newer draft entered after leaving and returning", async () => {
    let finish!: (doc: MdWorkspace["documents"][number]) => void;
    api.save.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    await open(); fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    fireEvent.change(screen.getByLabelText("作品设定 Markdown"), { target: { value: "提交的旧草稿" } });
    fireEvent.click(screen.getByRole("button", { name: "保存文字" }));
    fireEvent.click(screen.getByRole("button", { name: "3. 本章剧本" }));
    fireEvent.click(screen.getByRole("button", { name: "2. 作品设定" }));
    fireEvent.change(screen.getByLabelText("作品设定 Markdown"), { target: { value: "返回后新输入" } });
    await act(async () => finish({ id: "s", kind: "settings", pageNo: null, markdown: "提交的旧草稿", revision: 1, stale: false, issues: [], updatedAt: 1 }));
    expect((screen.getByLabelText("作品设定 Markdown") as HTMLTextAreaElement).value).toBe("返回后新输入");
    expect(JSON.parse(localStorage.getItem("comic-md:draft:p:w:c1:settings:")!).markdown).toBe("返回后新输入");
  });
  it("excludes unsaved prompt edits from batch image rendering", async () => {
    data.documents = [{ id: "d", kind: "page_prompt", pageNo: 1, markdown: "已保存Prompt", revision: 1, stale: false, issues: [], updatedAt: 1 }];
    await open(); fireEvent.click(screen.getByRole("button", { name: "5. 每页 Prompt" }));
    fireEvent.change(screen.getByLabelText("本页 Prompt Markdown"), { target: { value: "尚未保存的修改" } });
    fireEvent.click(screen.getByRole("button", { name: "6. 漫画" }));
    expect((screen.getByRole("button", { name: "生成剩余页" }) as HTMLButtonElement).disabled).toBe(true);
    expect(api.render).not.toHaveBeenCalled();
  });
  it("keeps successful generation history collapsed without removing its raw Markdown", async () => {
    data.jobs = [{ id: "done", kind: "script", status: "succeeded", message: "剧本已经生成", outputMarkdown: "完整剧本原始文字", completedPages: 0, totalPages: 0, createdAt: 1 }];
    await open();
    expect(screen.queryByLabelText("当前生成状态")).toBeNull();
    const history = screen.getByLabelText("生成记录") as HTMLDetailsElement;
    expect(history.open).toBe(false);
    expect((screen.getByLabelText("生成原始 Markdown") as HTMLTextAreaElement).value).toBe("完整剧本原始文字");
    expect(screen.queryByText("从小说到漫画，先把故事写好")).toBeNull();
    expect(screen.getByText("小说原文资产由统一管理器维护；本页保存的是独立的漫画改编稿、分镜和图片。管理原文不会自动覆盖这些成果。")).toBeTruthy();
  });
});
