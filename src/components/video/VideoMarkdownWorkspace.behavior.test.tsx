// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

const state = vi.hoisted(() => ({ assets: [] as Array<Record<string, unknown>> }));
const api = vi.hoisted(() => ({ llmChat: vi.fn(), saveDocumentVersion: vi.fn(), confirmAction: vi.fn() }));
const pickers = vi.hoisted(() => ({ byTitle: {} as Record<string, (asset: Record<string, unknown>) => void>, filters: {} as Record<string, (asset: Record<string, unknown>) => boolean> }));
const novelManagers = vi.hoisted(() => ({
  onSelect: undefined as undefined | ((selection: Record<string, unknown>) => void),
  onChanged: undefined as undefined | ((change: Record<string, unknown>) => void),
  manageOnSelect: undefined as undefined | ((selection: Record<string, unknown>) => void),
}));

vi.mock("../../store/useProjectStore", () => ({
  useProjectStore: (selector: (value: Record<string, unknown>) => unknown) => selector({
    activeId: "p",
    projects: [{ id: "p", name: "测试视频", storyStyle: "悬疑", artStyle: "电影写实", aspectRatio: "16:9", videoModel: "grok-imagine-video", videoResolution: "720p" }],
  }),
}));

vi.mock("../../store/useLibraryStore", () => ({
  useLibraryStore: (selector: (value: Record<string, unknown>) => unknown) => selector({ assets: state.assets, tasks: [] }),
}));

vi.mock("../../lib/ipc", () => ({ llmChat: api.llmChat }));
vi.mock("../../lib/confirm", () => ({ confirmAction: api.confirmAction }));
vi.mock("../../lib/documents", async (original) => ({
  ...await original<typeof import("../../lib/documents")>(),
  saveDocumentVersion: api.saveDocumentVersion,
}));
vi.mock("../AssetPicker", () => ({ default: ({ title, open, onPick, filter }: { title: string; open: boolean; onPick: (asset: Record<string, unknown>) => void; filter: (asset: Record<string, unknown>) => boolean }) => { if (open) { pickers.byTitle[title] = onPick; pickers.filters[title] = filter; } return null; } }));
vi.mock("../novel/NovelAssetManager", () => ({ default: ({ open, mode, onSelect, onChanged }: { open: boolean; mode: "select" | "manage"; onSelect?: (selection: Record<string, unknown>) => void; onChanged?: (change: Record<string, unknown>) => void }) => { if (open && mode === "select") novelManagers.onSelect = onSelect; if (open && mode === "manage") { novelManagers.onChanged = onChanged; novelManagers.manageOnSelect = onSelect; } return null; } }));
vi.mock("./VideoStoryboardEditor", () => ({ default: () => null }));

import VideoMarkdownWorkspace from "./VideoMarkdownWorkspace";

function textAsset(id: string, source: string, params: Record<string, unknown>, createdAt: number) {
  return { asset: { id, kind: "text", path: `${id}.md` }, source, projectId: "p", params, createdAt };
}

const provenance = (parentAssetIds: string[], type = "manual") => ({
  schemaVersion: 1,
  sourceMaterials: [],
  parentAssetIds,
  revision: { type },
  recordedAt: 1,
});

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  pickers.byTitle = {};
  pickers.filters = {};
  novelManagers.onSelect = undefined;
  novelManagers.onChanged = undefined;
  novelManagers.manageOnSelect = undefined;
  state.assets = [
    textAsset("director-v1", "导演规划 · 第1章", {
      text: "# 改编规划\n\n旧规划",
      title: "导演规划 · 第1章",
      documentType: "director",
      documentId: "director-doc",
      version: 1,
      changeType: "manual",
      agentId: "director",
      videoWorkflowId: "video:novel:book:chapter-1",
      novelWorkId: "book",
      novelChapterId: "chapter-1",
      chapterNo: 1,
      provenance: provenance(["source-v1"]),
    }, 2),
    textAsset("source-v1", "视频原始资料 · 第1章", {
      text: "# 原始资料\n\n## 类型\n小说\n\n## 正文\n第一章正文",
      title: "视频原始资料 · 第1章",
      documentType: "document",
      documentId: "source-doc",
      version: 1,
      changeType: "manual",
      agentId: "source",
      videoWorkflowId: "video:novel:book:chapter-1",
      sourceKind: "novel_chapter",
      novelWorkId: "book",
      novelChapterId: "chapter-1",
      novelChapterRevisionId: "revision-1",
      chapterNo: 1,
      novelSourceReference: { projectId: "p", novelWorkId: "book", novelChapterId: "chapter-1", novelChapterRevisionId: "revision-1", chapterNo: 1, title: "夜雨", content: "第一章正文" },
      provenance: provenance([]),
    }, 1),
  ];
  api.llmChat
    .mockResolvedValueOnce("# 改编规划\n\n## 输入边界\n严格停在原文结尾。\n\n## 项目硬约束\n16:9，无声音字幕。\n\n## 核心戏剧判断\n人物因选择承担后果。\n\n## 人物弧与关系\n保持人物关系不变。\n\n## 叙事节拍\n进入状态 → 选择 → 结果 → 承接。\n\n## 核心视觉母题\n用原文既有道具贯穿。\n\n## 视觉策略\n克制写实。\n\n## 风险与自检\n无越界续写。")
    .mockResolvedValueOnce("# 产物质量审查\n【结论】通过\n【总分】88\n【剧情吸引力】85 | 因果清晰\n【视觉独特性】80 | 使用原文视觉母题\n【原文忠实度】92 | 未越界\n【人物与情感】86 | 动机成立\n【可执行性】88 | 可拍\n【一致性】90 | 一致\n【亮点】人物选择明确\n【阻断问题】无\n【一般问题】无\n【改进建议】保持克制");
  api.confirmAction.mockResolvedValue(true);
  api.saveDocumentVersion.mockResolvedValue(textAsset("director-v2", "导演规划 · 第1章", {
    text: "# 改编规划\n\n## 输入边界\n严格停在原文结尾。\n\n## 项目硬约束\n16:9，无声音字幕。\n\n## 核心戏剧判断\n人物因选择承担后果。\n\n## 人物弧与关系\n保持人物关系不变。\n\n## 叙事节拍\n进入状态 → 选择 → 结果 → 承接。\n\n## 核心视觉母题\n用原文既有道具贯穿。\n\n## 视觉策略\n克制写实。\n\n## 风险与自检\n无越界续写。",
    title: "导演规划 · 第1章",
    documentType: "director",
    documentId: "director-doc",
    version: 2,
    changeType: "ai_optimized",
    agentId: "director",
    videoWorkflowId: "video:novel:book:chapter-1",
    novelWorkId: "book",
    novelChapterId: "chapter-1",
    chapterNo: 1,
    provenance: provenance(["source-v1"], "ai_optimized"),
  }, 3));
});

it("imports a current-project video work as a workflow-level Agent reference", async () => {
  state.assets.push({ asset: { id: "video-work", kind: "video", path: "D:/video-work.mp4", durationS: 8 }, source: "参考短片", projectId: "p", model: "video-model", params: {}, createdAt: 3 });
  render(<VideoMarkdownWorkspace onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);

  fireEvent.click(screen.getByRole("button", { name: "导入视频作品" }));
  const onPick = pickers.byTitle["导入当前项目的视频作品参考"];
  expect(onPick).toBeTypeOf("function");
  onPick(state.assets.find((asset) => (asset.asset as { id: string }).id === "video-work")!);

  expect((await screen.findByLabelText("短剧视频作品参考")).textContent).toContain("参考短片 · 8秒");
  expect(localStorage.getItem("video-md:work-video-references:p")).toContain("video-work");
});

afterEach(cleanup);

describe("VideoMarkdownWorkspace editing loop", () => {
  it("uses a confirmed novel revision as an editable, persistently labelled source snapshot", async () => {
    api.saveDocumentVersion.mockResolvedValue(textAsset("source-v2", "视频原始资料 · 第2章", {
      text: "# 原始资料\n\n## 类型\n小说章节\n\n## 正文\n第二章正文",
      title: "视频原始资料 · 第2章",
      documentType: "document",
      documentId: "source-doc-2",
      version: 1,
      changeType: "manual",
      agentId: "source",
      videoWorkflowId: "video:novel:book:chapter-2",
      sourceKind: "novel_chapter",
      novelWorkId: "book",
      novelChapterId: "chapter-2",
      novelChapterRevisionId: "revision-2",
      chapterNo: 2,
      novelSourceReference: { projectId: "p", novelWorkId: "book", novelChapterId: "chapter-2", novelChapterRevisionId: "revision-2", chapterNo: 2, workTitle: "雨夜来信", title: "转机", content: "第二章正文" },
      provenance: provenance([]),
    }, 4));
    render(<VideoMarkdownWorkspace onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "引用/采用小说章节为短剧快照" }));
    novelManagers.onSelect?.({ projectId: "p", novelWorkId: "book", novelChapterId: "chapter-2", novelChapterRevisionId: "revision-2", revisionNo: 3, sourceAssetId: "novel-r2", chapterNo: 2, workTitle: "雨夜来信", title: "转机", content: "第二章正文" });

    await waitFor(() => expect(screen.getByLabelText("原始资料 Markdown")).toHaveProperty("value", "第二章正文"));
    expect(screen.getByText(/雨夜来信/)).toBeTruthy();
    expect(screen.getByText(/第2章.*转机.*revision-2/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "保存初稿" }));

    await waitFor(() => expect(api.saveDocumentVersion).toHaveBeenCalledWith(expect.objectContaining({
      metadata: expect.objectContaining({
        sourceKind: "novel_chapter",
        novelWorkId: "book",
        novelChapterId: "chapter-2",
        novelChapterRevisionId: "revision-2",
        chapterNo: 2,
        sourceAssetId: "novel-r2",
        novelSourceReference: expect.objectContaining({ workTitle: "雨夜来信", title: "转机", content: "第二章正文" }),
      }),
      provenance: expect.objectContaining({
        originalInput: "第二章正文",
        sourceMaterials: [expect.objectContaining({ label: expect.stringContaining("第2章") })],
        parentAssetIds: ["novel-r2"],
      }),
    })));
  });

  it("keeps the short-drama snapshot unchanged when shared novel-asset management saves a revision", async () => {
    render(<VideoMarkdownWorkspace onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    const originalSnapshot = (screen.getByLabelText("原始资料 Markdown") as HTMLTextAreaElement).value;

    fireEvent.click(screen.getByRole("button", { name: "管理小说原文资产" }));
    expect(novelManagers.manageOnSelect).toBeUndefined();
    novelManagers.onChanged?.({ projectId: "p", novelWorkId: "book", novelChapterId: "chapter-1", novelChapterRevisionId: "revision-2" });

    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("当前短剧原文快照未改动"));
    expect(screen.getByLabelText("原始资料 Markdown")).toHaveProperty("value", originalSnapshot);
    expect(api.saveDocumentVersion).not.toHaveBeenCalled();
  });

  it("does not replace an unsaved draft in the selected chapter until the user confirms it", async () => {
    state.assets.push(textAsset("source-b", "视频原始资料 · 第2章", {
      text: "# 原始资料\n\n## 类型\n小说章节\n\n## 正文\n第二章已保存正文",
      title: "视频原始资料 · 第2章",
      documentType: "document",
      documentId: "source-doc-b",
      version: 1,
      changeType: "manual",
      agentId: "source",
      videoWorkflowId: "video:novel:book:chapter-2",
      sourceKind: "novel_chapter",
      novelWorkId: "book",
      novelChapterId: "chapter-2",
      novelChapterRevisionId: "revision-2",
      chapterNo: 2,
      provenance: provenance([]),
    }, 3));
    localStorage.setItem("video-md:draft:p:video:novel:book:chapter-2:source:source-b", JSON.stringify({
      text: "# 原始资料\n\n## 类型\n小说章节\n\n## 正文\n第二章未保存正文",
      optimizationInstruction: "",
      changeType: "manual",
      sourceKind: "novel",
      dependencyMode: "current",
    }));
    localStorage.setItem("video-md:last-workflow:p", "video:novel:book:chapter-1");
    api.confirmAction.mockResolvedValueOnce(false);
    render(<VideoMarkdownWorkspace onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "引用/采用小说章节为短剧快照" }));
    novelManagers.onSelect?.({ projectId: "p", novelWorkId: "book", novelChapterId: "chapter-2", novelChapterRevisionId: "revision-2", revisionNo: 3, chapterNo: 2, workTitle: "雨夜来信", title: "转机", content: "第二章正文" });

    await waitFor(() => expect(api.confirmAction).toHaveBeenCalledOnce());
    expect(screen.getByLabelText("原始资料 Markdown")).toHaveProperty("value", "# 原始资料\n\n## 类型\n小说\n\n## 正文\n第一章正文");
    expect(localStorage.getItem("video-md:draft:p:video:novel:book:chapter-2:source:source-b")).toContain("第二章未保存正文");
  });

  it("clears novel lineage when manual source text is saved and reopened", async () => {
    let savedManual: Record<string, unknown> | undefined;
    api.saveDocumentVersion.mockImplementationOnce(async (input: Record<string, any>) => {
      savedManual = textAsset("source-manual-v2", "视频原始资料 · 第1章", {
        ...input.metadata,
        text: input.text,
        title: "视频原始资料 · 第1章",
        documentType: "document",
        documentId: "source-doc",
        version: 2,
        changeType: "manual",
        agentId: "source",
        provenance: input.provenance,
      }, 5);
      return savedManual;
    });
    const rendered = render(<VideoMarkdownWorkspace onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);

    fireEvent.change(screen.getByLabelText("原文来源类型"), { target: { value: "manual" } });
    fireEvent.change(screen.getByLabelText("原始资料 Markdown"), { target: { value: "手动补充的原文" } });
    fireEvent.click(screen.getByRole("button", { name: "保存 v2" }));

    await waitFor(() => expect(api.saveDocumentVersion).toHaveBeenCalledOnce());
    const request = api.saveDocumentVersion.mock.calls[0][0] as { metadata: Record<string, unknown>; provenance: Record<string, unknown> };
    expect(request.metadata).toMatchObject({ sourceKind: "manual" });
    expect(request.metadata).not.toHaveProperty("novelWorkId");
    expect(request.metadata).not.toHaveProperty("novelChapterId");
    expect(request.metadata).not.toHaveProperty("novelChapterRevisionId");
    expect(request.metadata).not.toHaveProperty("novelSourceReference");
    expect(request.provenance).toMatchObject({ sourceMaterials: [], parentAssetIds: [] });

    state.assets = [savedManual!, ...state.assets];
    rendered.unmount();
    render(<VideoMarkdownWorkspace onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    expect(screen.getByLabelText("原文来源类型")).toHaveProperty("value", "manual");
    expect(screen.queryByText(/^来源：/)).toBeNull();
    expect(screen.getByLabelText("原始资料 Markdown")).toHaveProperty("value", "# 原始资料\n\n## 类型\n手动原文\n\n## 正文\n手动补充的原文");
  });

  it("keeps the existing fixed snapshot flow for other project text assets", async () => {
    const otherText = textAsset("research-note", "人物关系笔记", {
      text: "其他文本资产正文",
      title: "人物关系笔记",
      documentType: "note",
      agentId: "research",
    }, 4);
    const savedIdeaSource = textAsset("idea-source", "视频原始资料 · 脑洞", {
      text: "# 原始资料\n\n## 类型\n脑洞\n\n## 正文\n旧脑洞正文",
      title: "视频原始资料 · 脑洞",
      documentType: "document",
      agentId: "source",
      videoWorkflowId: "video:idea:p",
      sourceKind: "idea",
    }, 4);
    state.assets.push(otherText, savedIdeaSource);
    localStorage.setItem("video-md:last-workflow:p", "video:novel:book:chapter-1");
    render(<VideoMarkdownWorkspace onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "从其他文本资产选择" }));
    const onPick = pickers.byTitle["选择其他文本原文资产"];
    const filter = pickers.filters["选择其他文本原文资产"];
    expect(onPick).toBeTypeOf("function");
    expect(filter(otherText)).toBe(true);
    expect(filter(savedIdeaSource)).toBe(true);
    expect(filter(state.assets.find((asset) => (asset.asset as { id: string }).id === "source-v1")!)).toBe(false);
    expect(filter(state.assets.find((asset) => (asset.asset as { id: string }).id === "director-v1")!)).toBe(false);
    onPick(otherText);

    await waitFor(() => expect(screen.getByLabelText("原始资料 Markdown")).toHaveProperty("value", "其他文本资产正文"));
    fireEvent.click(screen.getByRole("button", { name: "保存 v2" }));
    await waitFor(() => expect(api.saveDocumentVersion).toHaveBeenCalledWith(expect.objectContaining({
      metadata: expect.objectContaining({ sourceKind: "manual", sourceAssetId: "research-note" }),
      provenance: expect.objectContaining({ parentAssetIds: ["research-note"] }),
    })));
    const request = api.saveDocumentVersion.mock.calls[0][0] as { metadata: Record<string, unknown> };
    expect(request.metadata).not.toHaveProperty("novelWorkId");
    expect(request.metadata).not.toHaveProperty("novelChapterRevisionId");
  });

  it("directly saves a reviewed optimization and its existing downstream chain", async () => {
    render(<VideoMarkdownWorkspace llmModel="test-llm" onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    fireEvent.click(screen.getByRole("tab", { name: /改编规划/ }));
    expect(screen.getByLabelText("改编规划 Markdown")).toHaveProperty("value", "# 改编规划\n\n旧规划");

    fireEvent.change(screen.getByLabelText("LLM 优化要求"), { target: { value: "加强因果，但保持人物关系" } });
    fireEvent.click(screen.getByRole("button", { name: "AI 优化" }));

    await waitFor(() => expect(api.llmChat).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(api.saveDocumentVersion).toHaveBeenCalledWith(expect.objectContaining({
      text: "# 改编规划\n\n## 输入边界\n严格停在原文结尾。\n\n## 项目硬约束\n16:9，无声音字幕。\n\n## 核心戏剧判断\n人物因选择承担后果。\n\n## 人物弧与关系\n保持人物关系不变。\n\n## 叙事节拍\n进入状态 → 选择 → 结果 → 承接。\n\n## 核心视觉母题\n用原文既有道具贯穿。\n\n## 视觉策略\n克制写实。\n\n## 风险与自检\n无越界续写。",
      parent: expect.objectContaining({ asset: expect.objectContaining({ id: "director-v1" }) }),
      changeType: "ai_optimized",
      revisionInstruction: "加强因果，但保持人物关系",
      metadata: expect.objectContaining({ videoWorkflowId: "video:novel:book:chapter-1", novelWorkId: "book", chapterNo: 1 }),
      provenance: expect.objectContaining({ parentAssetIds: ["source-v1"] }),
    })));
    expect(screen.getByRole("status").textContent).toContain("已直接保存为新版本");
  });

  it("optimizes and directly saves an existing downstream script with the new director as its parent", async () => {
    const scriptV1 = "# 视频剧本\n\n## 改编边界\n停在原文结尾。\n\n## 人物当前状态\n人物保持原关系。\n\n## 分场剧本\n### 第1场｜院子｜白天\n人物做出选择。\n\n## 编剧自检\n无越界续写。";
    const scriptV2 = "# 视频剧本\n\n## 改编边界\n严格停在原文结尾。\n\n## 人物当前状态\n人物保持原关系并承担选择后果。\n\n## 分场剧本\n### 第1场｜院子｜白天\n人物经过犹豫后做出选择，结果清晰。\n\n## 编剧自检\n因果完整，无越界续写。";
    state.assets.unshift(textAsset("script-v1", "剧本 · 第1章", {
      text: scriptV1,
      title: "剧本 · 第1章",
      documentType: "script",
      documentId: "script-doc",
      version: 1,
      changeType: "manual",
      agentId: "writer",
      videoWorkflowId: "video:novel:book:chapter-1",
      novelWorkId: "book",
      novelChapterId: "chapter-1",
      chapterNo: 1,
      provenance: provenance(["source-v1", "director-v1"]),
    }, 3));
    const optimizedDirector = "# 改编规划\n\n## 输入边界\n严格停在原文结尾。\n\n## 项目硬约束\n16:9，无声音字幕。\n\n## 核心戏剧判断\n人物因选择承担后果。\n\n## 人物弧与关系\n保持人物关系不变。\n\n## 叙事节拍\n进入状态 → 选择 → 结果 → 承接。\n\n## 核心视觉母题\n用原文既有道具贯穿。\n\n## 视觉策略\n克制写实。\n\n## 风险与自检\n无越界续写。";
    const passedReview = "# 产物质量审查\n【结论】通过\n【总分】88\n【剧情吸引力】85 | 因果清晰\n【视觉独特性】80 | 使用原文视觉母题\n【原文忠实度】92 | 未越界\n【人物与情感】86 | 动机成立\n【可执行性】88 | 可拍\n【一致性】90 | 一致\n【亮点】人物选择明确\n【阻断问题】无\n【一般问题】无\n【改进建议】保持克制";
    api.llmChat.mockReset()
      .mockResolvedValueOnce(optimizedDirector)
      .mockResolvedValueOnce(passedReview)
      .mockResolvedValueOnce(scriptV2)
      .mockResolvedValueOnce(passedReview);
    api.saveDocumentVersion.mockImplementation(async (input) => textAsset(
      input.agentId === "director" ? "director-v2" : "script-v2",
      input.title,
      {
        text: input.text,
        title: input.title,
        documentType: input.documentType,
        documentId: input.agentId === "director" ? "director-doc" : "script-doc",
        version: 2,
        changeType: "ai_optimized",
        agentId: input.agentId,
        videoWorkflowId: "video:novel:book:chapter-1",
        novelWorkId: "book",
        novelChapterId: "chapter-1",
        chapterNo: 1,
        provenance: provenance(input.provenance.parentAssetIds, "ai_optimized"),
      },
      4,
    ));

    render(<VideoMarkdownWorkspace llmModel="test-llm" onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    fireEvent.click(screen.getByRole("tab", { name: /改编规划/ }));
    fireEvent.change(screen.getByLabelText("LLM 优化要求"), { target: { value: "加强选择与后果" } });
    fireEvent.click(screen.getByRole("button", { name: "AI 优化" }));

    await waitFor(() => expect(api.saveDocumentVersion).toHaveBeenCalledTimes(2));
    expect(api.llmChat).toHaveBeenCalledTimes(4);
    expect(api.saveDocumentVersion).toHaveBeenNthCalledWith(1, expect.objectContaining({
      agentId: "director",
      parent: expect.objectContaining({ asset: expect.objectContaining({ id: "director-v1" }) }),
      provenance: expect.objectContaining({ parentAssetIds: ["source-v1"] }),
    }));
    expect(api.saveDocumentVersion).toHaveBeenNthCalledWith(2, expect.objectContaining({
      agentId: "writer",
      parent: expect.objectContaining({ asset: expect.objectContaining({ id: "script-v1" }) }),
      provenance: expect.objectContaining({ parentAssetIds: ["source-v1", "director-v2"] }),
    }));
    expect(String(api.llmChat.mock.calls[0][1])).toContain("同一视频工作区的其他已保存产物与媒体元数据");
    expect(String(api.llmChat.mock.calls[0][1])).toContain(scriptV1);
    expect(String(api.llmChat.mock.calls[2][1])).toContain(optimizedDirector);
    expect(screen.getByRole("status").textContent).toContain("改编规划 → 剧本");
  });

  it("keeps a historical branch on its original dependencies instead of creating a false current version", async () => {
    const sourceV1 = state.assets.find((asset) => (asset.asset as { id: string }).id === "source-v1")!;
    const sourceV2 = textAsset("source-v2", "视频原始资料 · 第1章", {
      ...(sourceV1.params as Record<string, unknown>),
      text: "# 原始资料\n\n## 类型\n小说\n\n## 正文\n第一章修订正文",
      version: 2,
      provenance: provenance([]),
    }, 3);
    const directorV1 = state.assets.find((asset) => (asset.asset as { id: string }).id === "director-v1")!;
    const directorV2Text = "# 改编规划\n\n当前规划";
    const directorV2 = textAsset("director-v2-current", "导演规划 · 第1章", {
      ...(directorV1.params as Record<string, unknown>),
      text: directorV2Text,
      version: 2,
      provenance: provenance(["source-v2"]),
      agentReview: "# 产物质量审查\n【结论】通过\n【总分】86\n【阻断问题】无\n【改进建议】无",
      agentReviewStatus: "passed",
      agentReviewScore: 86,
      agentReviewedText: directorV2Text,
      agentReviewPolicyVersion: 2,
    }, 4);
    state.assets = [directorV2, sourceV2, directorV1, sourceV1];
    api.saveDocumentVersion.mockResolvedValue(textAsset("director-branch", "导演规划 · 第1章 · 历史分支", {
      ...(directorV1.params as Record<string, unknown>), version: 3, videoBranch: true,
    }, 5));

    render(<VideoMarkdownWorkspace llmModel="test-llm" onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    fireEvent.click(screen.getByRole("tab", { name: /改编规划/ }));
    fireEvent.change(screen.getByLabelText("保存版本"), { target: { value: "director-v1" } });
    await waitFor(() => expect(screen.getByRole("button", { name: "按原依赖创建分支" })).toBeTruthy());
    expect(screen.getByLabelText("改编规划 Markdown")).toHaveProperty("readOnly", true);

    fireEvent.click(screen.getByRole("button", { name: "按原依赖创建分支" }));
    fireEvent.change(screen.getByLabelText("当前草稿"), { target: { value: "# 改编规划\n\n历史分支修改" } });
    fireEvent.click(screen.getByRole("button", { name: "保存历史分支" }));

    await waitFor(() => expect(api.saveDocumentVersion).toHaveBeenCalledWith(expect.objectContaining({
      parent: expect.objectContaining({ asset: expect.objectContaining({ id: "director-v1" }) }),
      metadata: expect.objectContaining({ videoBranch: true, dependencyMode: "preserve_history" }),
      provenance: expect.objectContaining({ parentAssetIds: ["source-v1"] }),
    })));
  });

  it("pins an edited version when a newer version arrives and requires an explicit migration strategy", async () => {
    const { rerender } = render(<VideoMarkdownWorkspace llmModel="test-llm" onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    fireEvent.click(screen.getByRole("tab", { name: /改编规划/ }));
    fireEvent.change(screen.getByLabelText("改编规划 Markdown"), { target: { value: "# 改编规划\n\n正在编辑的旧基线" } });

    const original = state.assets.find((asset) => (asset.asset as { id: string }).id === "director-v1")!;
    state.assets = [textAsset("director-v2-external", "导演规划 · 第1章", {
      ...(original.params as Record<string, unknown>),
      text: "# 改编规划\n\n外部写入的新版本",
      version: 2,
      provenance: provenance(["source-v1"]),
    }, 10), ...state.assets];
    rerender(<VideoMarkdownWorkspace llmModel="test-llm" onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);

    expect(screen.getByLabelText("改编规划 Markdown")).toHaveProperty("value", "# 改编规划\n\n正在编辑的旧基线");
    expect(screen.getByRole("alert").textContent).toContain("编辑期间出现了新的当前生产版");
    expect(screen.getByRole("button", { name: "保存 v3" })).toHaveProperty("disabled", true);

    fireEvent.click(screen.getByRole("button", { name: "迁移到最新依赖…" }));
    expect(screen.getByRole("button", { name: "保存迁移版本" })).toHaveProperty("disabled", false);
  });

  it("keeps a saved novel snapshot stable when the chapter receives a newer revision", async () => {
    const videoSource = state.assets.find((asset) => (asset.asset as { id: string }).id === "source-v1")!;
    videoSource.params = { ...(videoSource.params as Record<string, unknown>), sourceAssetId: "novel-r1", novelChapterRevisionId: "r1" };
    state.assets = [
      textAsset("novel-r2", "第一章 · 最新修订", { text: "第二版章节正文", documentType: "novel", agentId: "novel_source", sourceKind: "novel_chapter", novelWorkId: "book", novelChapterId: "chapter-1", novelChapterRevisionId: "r2", chapterNo: 1 }, 20),
      textAsset("novel-r1", "第一章 · 旧修订", { text: "第一版章节正文", documentType: "novel", agentId: "novel_source", sourceKind: "novel_chapter", novelWorkId: "book", novelChapterId: "chapter-1", novelChapterRevisionId: "r1", chapterNo: 1 }, 0),
      ...state.assets,
    ];

    render(<VideoMarkdownWorkspace llmModel="test-llm" onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    expect(screen.queryByRole("tab", { name: /原始资料 需更新/ })).toBeNull();
    expect(screen.queryByRole("button", { name: "载入章节最新版" })).toBeNull();
    expect(screen.getByLabelText("原始资料 Markdown")).toHaveProperty("value", "# 原始资料\n\n## 类型\n小说\n\n## 正文\n第一章正文");
  });

  it("requires an explicit choice when a project contains multiple video workspaces", () => {
    const secondSource = textAsset("source-2", "视频原始资料 · 第2章", {
      ...(state.assets.find((asset) => (asset.asset as { id: string }).id === "source-v1")!.params as Record<string, unknown>),
      documentId: "source-doc-2", videoWorkflowId: "video:novel:book:chapter-2", novelChapterId: "chapter-2", chapterNo: 2,
    }, 30);
    state.assets = [secondSource, ...state.assets];
    render(<VideoMarkdownWorkspace llmModel="test-llm" onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    expect(screen.getByRole("heading", { name: "请选择视频工作区" })).toBeTruthy();
    expect(screen.queryByRole("navigation", { name: "短剧生产阶段" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /第2章/ }));
    expect(screen.getByRole("navigation", { name: "短剧生产阶段" })).toBeTruthy();
  });

  it("keeps a draft visible in the stage track after the user moves to another stage", () => {
    render(<VideoMarkdownWorkspace llmModel="test-llm" onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    fireEvent.click(screen.getByRole("tab", { name: /改编规划/ }));
    fireEvent.change(screen.getByLabelText("改编规划 Markdown"), { target: { value: "# 改编规划\n\n跨阶段保留的草稿" } });
    fireEvent.click(screen.getByRole("tab", { name: /视频锚点/ }));
    expect(screen.getByRole("tab", { name: /改编规划 草稿未保存/ })).toBeTruthy();
    expect(screen.getByText("另有未保存草稿，不参与当前下游生产")).toBeTruthy();
  });

  it("links the selected stage tab to its panel", () => {
    render(<VideoMarkdownWorkspace llmModel="test-llm" onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    const sourceTab = screen.getByRole("tab", { name: /原始资料/ });
    const panel = screen.getByRole("tabpanel");
    expect(panel.getAttribute("id")).toBe(sourceTab.getAttribute("aria-controls"));
    expect(panel.getAttribute("aria-labelledby")).toBe(sourceTab.id);
    for (const tab of screen.getByRole("tablist", { name: "短剧生产阶段" }).querySelectorAll<HTMLElement>("[role='tab']")) {
      const controls = tab.getAttribute("aria-controls") ?? "";
      expect(document.getElementById(controls), `Missing panel for ${controls}`).toBeTruthy();
    }

    fireEvent.keyDown(sourceTab, { key: "ArrowRight" });
    const directorTab = screen.getByRole("tab", { name: /改编规划/ });
    expect(document.activeElement).toBe(directorTab);
    expect(screen.getByRole("tabpanel").getAttribute("aria-labelledby")).toBe(directorTab.id);
  });

  it("traps focus in the discard dialog and Escape closes it without discarding the draft", async () => {
    render(<VideoMarkdownWorkspace llmModel="test-llm" onEditPrompt={vi.fn()} onSendToVideo={vi.fn()} />);
    fireEvent.click(screen.getByRole("tab", { name: /改编规划/ }));
    const draft = screen.getByLabelText("改编规划 Markdown");
    fireEvent.change(draft, { target: { value: "# 改编规划\n\n还不能丢失的草稿" } });
    const trigger = screen.getByRole("button", { name: "放弃草稿并恢复已保存内容…" });
    fireEvent.click(trigger);

    const dialog = screen.getByRole("dialog", { name: "放弃当前草稿？" });
    const continueEditing = screen.getByRole("button", { name: "继续编辑" });
    const discard = screen.getByRole("button", { name: "放弃草稿" });
    await waitFor(() => expect(document.activeElement).toBe(continueEditing));

    fireEvent.keyDown(dialog, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(discard);
    fireEvent.keyDown(dialog, { key: "Tab" });
    expect(document.activeElement).toBe(continueEditing);

    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.activeElement).toBe(trigger);
    expect(draft).toHaveProperty("value", "# 改编规划\n\n还不能丢失的草稿");
  });
});
