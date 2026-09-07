import { beforeEach, describe, expect, it, vi } from "vitest";
const invoke = vi.hoisted(() => vi.fn());
vi.mock("../logger", () => ({ loggedInvoke: invoke }));
import { comicMdDocumentSave, comicMdGenerate, comicMdRender, comicMdOptimize, comicMdSync, comicMdRenderOptionsSave, comicMdDocumentHistory, comicMdExport, comicMdWorkspaceGet, comicMdWorkVisualExtract, comicMdWorkVisualImport, comicMdWorkVisualSave, mdStageBlock, type MdWorkspace } from "./markdownApi";
const scope = { projectId: "p", novelWorkId: "w", chapterId: "c" };
describe("Markdown IPC", () => {
  beforeEach(() => invoke.mockReset());
  it("binds linked updates to the exact reviewed plan and passes explicit acknowledgment only when requested", async () => {
    await comicMdSync({ ...scope, expectedPlanFingerprint: "reviewed-plan" });
    expect(invoke).toHaveBeenLastCalledWith("comic_md_sync", { input: { ...scope, expectedPlanFingerprint: "reviewed-plan" } });
    await comicMdDocumentSave({ ...scope, kind: "settings", markdown: "已核对", expectedRevision: 2, acknowledgeUpdates: true });
    expect(invoke).toHaveBeenLastCalledWith("comic_md_document_save", { input: { ...scope, kind: "settings", markdown: "已核对", expectedRevision: 2, acknowledgeUpdates: true } });
  });
  it("preserves exact Markdown and optimistic revision through an input envelope", async () => {
    const input = { ...scope, kind: "page_prompt" as const, pageNo: 3, markdown: "# 第3页\n\n中文 `literal`\n", expectedRevision: 7 };
    await comicMdDocumentSave(input); expect(invoke).toHaveBeenCalledWith("comic_md_document_save", { input });
  });
  it("uses source identity for text and document revisions for explicit rendering", async () => {
    await comicMdGenerate({ ...scope, stage: "page_prompts", expectedSourceRevisionId: "r" });
    await comicMdRender({ ...scope, pages: [{ documentId: "d", revision: 8 }], expectedRenderOptionsRevision: 0 });
    expect(invoke.mock.calls).toEqual([["comic_md_generate", { input: { ...scope, stage: "page_prompts", expectedSourceRevisionId: "r" } }], ["comic_md_render", { input: { ...scope, pages: [{ documentId: "d", revision: 8 }], expectedRenderOptionsRevision: 0 } }]]);
  });
  it("keeps reads, history and export scoped", async () => {
    await comicMdWorkspaceGet(scope); await comicMdDocumentHistory({ ...scope, documentId: "d" }); await comicMdExport({ ...scope, documentIds: ["d"] });
    expect(invoke.mock.calls.map((call) => call[0])).toEqual(["comic_md_workspace_get", "comic_md_document_history", "comic_md_export"]);
    expect(invoke.mock.calls.every((call) => call[1].input.chapterId === "c")).toBe(true);
  });
  it("keeps work visual import, save and multimodal extraction at project plus novel-work scope", async () => {
    const work = { projectId: "p", novelWorkId: "w" };
    await comicMdWorkVisualImport({ ...work, paths: ["D:/reference.png"] });
    await comicMdWorkVisualSave({ ...work, constitutionMarkdown: "## 画风\n水墨", references: [{ assetId: "a", role: "style", weight: 0.7, sortOrder: 0 }], expectedRevision: 2 });
    await comicMdWorkVisualExtract({ ...work, expectedRevision: 3, instruction: "只参考线条" });
    expect(invoke.mock.calls).toEqual([
      ["comic_md_work_visual_import", { input: { ...work, paths: ["D:/reference.png"] } }],
      ["comic_md_work_visual_save", { input: { ...work, constitutionMarkdown: "## 画风\n水墨", references: [{ assetId: "a", role: "style", weight: 0.7, sortOrder: 0 }], expectedRevision: 2 } }],
      ["comic_md_work_visual_extract", { input: { ...work, expectedRevision: 3, instruction: "只参考线条" } }],
    ]);
  });
  it("preserves optimization instruction and revision-bound target envelopes", async () => {
    await comicMdDocumentSave({ ...scope, kind: "script", markdown: "剧本", optimizationInstruction: "要求\n原样", expectedRevision: 4 });
    await comicMdOptimize({ ...scope, targets: [{ documentId: "d", revision: 5 }], instruction: "要求\n原样" });
    expect(invoke.mock.calls[0]).toEqual(["comic_md_document_save", { input: { ...scope, kind: "script", markdown: "剧本", optimizationInstruction: "要求\n原样", expectedRevision: 4 } }]);
    expect(invoke.mock.calls[1]).toEqual(["comic_md_optimize", { input: { ...scope, targets: [{ documentId: "d", revision: 5 }], instruction: "要求\n原样" } }]);
  });
  it("saves chapter injection with its expected options revision", async () => {
    await comicMdRenderOptionsSave({ ...scope, promptInjection: "优先要求\n保留换行", expectedRevision: 7 });
    expect(invoke).toHaveBeenCalledWith("comic_md_render_options_save", { input: { ...scope, promptInjection: "优先要求\n保留换行", expectedRevision: 7 } });
  });
  it("passes an optional single-image rerun injection without changing the page envelope", async () => {
    await comicMdRender({ ...scope, pages: [{ documentId: "d", revision: 8 }], expectedRenderOptionsRevision: 3, rerunPromptInjection: "只修改雨伞颜色" });
    expect(invoke).toHaveBeenCalledWith("comic_md_render", { input: { ...scope, pages: [{ documentId: "d", revision: 8 }], expectedRenderOptionsRevision: 3, rerunPromptInjection: "只修改雨伞颜色" } });
  });
  it("prevents generation from stale or incomplete predecessors", () => {
    const workspace: MdWorkspace = { sourceRevisionId: "r", sourceContent: "正文", documents: [], jobs: [], images: [], textReady: false, imageReady: false };
    expect(mdStageBlock("settings", workspace)).toBeNull(); expect(mdStageBlock("script", workspace)).toContain("作品设定");
    workspace.documents.push({ id: "s", kind: "settings", pageNo: null, markdown: "完整设定", revision: 1, stale: false, issues: [], updatedAt: 1 });
    expect(mdStageBlock("script", workspace)).toBeNull(); workspace.documents[0].stale = true; expect(mdStageBlock("script", workspace)).toContain("作品设定");
  });
});
