import { loggedInvoke } from "../logger";

export interface MdScope { projectId: string; novelWorkId: string; chapterId: string }
export type MdKind = "settings" | "script" | "storyboard" | "page_prompt";
export type MdStage = "settings" | "script" | "storyboard" | "page_prompts";
export interface MdDocument { id: string; kind: MdKind; pageNo: number | null; markdown: string; optimizationInstruction?: string; revision: number; contentHash?: string; stale: boolean; staleReasons?: string[]; outOfPlan?: boolean; issues: string[]; updatedAt: number }
export interface MdJob { id: string; kind: MdStage | "images" | "optimize" | "sync"; status: "running" | "succeeded" | "failed" | "interrupted"; message: string | null; outputMarkdown: string | null; completedPages: number; totalPages: number; createdAt: number }
export interface MdImage { id: string; documentId: string; documentRevision: number; contentHash?: string; pageNo: number; path: string; promptInjection?: string; rerunPromptInjection?: string; visualProfileRevision?: number; visualReferenceSnapshot?: string; stale: boolean; fileAvailable?: boolean; createdAt: number }
export interface MdRenderOptions { promptInjection: string; revision: number }
export interface MdWorkVisualReference { assetId: string; path?: string; role: string; weight: number; sortOrder: number; note?: string; sha256?: string; fileAvailable: boolean }
export interface MdWorkVisualProfile { constitutionMarkdown: string; revision: number; references: MdWorkVisualReference[] }
/** A work-level style reference selected in the front end. */
export interface MdStyleReference { assetId: string; path: string; label: string; description?: string }
export interface MdSyncTarget { documentId: string; revision: number; kind: MdKind; pageNo: number | null; reasons: string[] }
export interface MdSyncPlan { fingerprint: string; targets: MdSyncTarget[]; missingPageNos: number[]; obsoletePageNos: number[]; blockedReason: string | null }
export interface MdAffectedChapter { chapterId: string; chapterNo: number; title: string | null; documentCount: number; reason: string }
export interface MdWorkspace { sourceRevisionId: string | null; sourceContent: string; documents: MdDocument[]; jobs: MdJob[]; images: MdImage[]; textReady: boolean; imageReady: boolean; renderOptions?: MdRenderOptions; workVisualProfile?: MdWorkVisualProfile; syncPlan?: MdSyncPlan; affectedChapters?: MdAffectedChapter[] }
export interface MdHistory { revision: number; markdown: string; optimizationInstruction?: string; createdAt: number }
const call = <T>(command: string, input: unknown) => loggedInvoke<T>(command, { input });
export const comicMdWorkspaceGet = (input: MdScope) => call<MdWorkspace>("comic_md_workspace_get", input);
export const comicMdWorkVisualGet = (input: Pick<MdScope, "projectId" | "novelWorkId">) => call<MdWorkVisualProfile>("comic_md_work_visual_get", input);
export const comicMdWorkVisualSave = (input: Pick<MdScope, "projectId" | "novelWorkId"> & { constitutionMarkdown: string; references: { assetId: string; role: string; weight: number; sortOrder: number; note?: string }[]; expectedRevision: number }) => call<MdWorkVisualProfile>("comic_md_work_visual_save", input);
export const comicMdWorkVisualExtract = (input: Pick<MdScope, "projectId" | "novelWorkId"> & { expectedRevision: number; instruction?: string }) => call<{ constitutionMarkdown: string; profileRevision: number; referenceAssetIds: string[] }>("comic_md_work_visual_extract", input);
export const comicMdWorkVisualImport = (input: Pick<MdScope, "projectId" | "novelWorkId"> & { paths: string[] }) => call<{ id: string; kind: "image"; path: string; format?: string }[]>("comic_md_work_visual_import", input);
export const comicMdDocumentSave = (input: MdScope & { kind: MdKind; pageNo?: number; markdown: string; optimizationInstruction?: string; expectedRevision: number | null; acknowledgeUpdates?: boolean }) => call<MdDocument>("comic_md_document_save", input);
export const comicMdDocumentHistory = (input: MdScope & { documentId: string }) => call<MdHistory[]>("comic_md_document_history", input);
export const comicMdGenerate = (input: MdScope & { stage: MdStage; expectedSourceRevisionId: string }) => call<MdJob>("comic_md_generate", input);
export const comicMdRender = (input: MdScope & { pages: { documentId: string; revision: number }[]; expectedRenderOptionsRevision: number; rerunPromptInjection?: string }) => call<MdJob>("comic_md_render", input);
export const comicMdOptimize = (input: MdScope & { targets: { documentId: string; revision: number }[]; instruction: string; allPages?: boolean }) => call<MdJob>("comic_md_optimize", input);
export const comicMdSync = (input: MdScope & { expectedPlanFingerprint: string }) => call<MdJob>("comic_md_sync", input);
export const comicMdRenderOptionsSave = (input: MdScope & { promptInjection: string; expectedRevision: number }) => call<MdRenderOptions>("comic_md_render_options_save", input);
export const comicMdExport = (input: MdScope & { documentIds?: string[] }) => call<{ path: string; files: string[] }>("comic_md_export", input);

export const mdLabels: Record<MdKind | "page_prompts" | "images" | "optimize" | "sync", string> = { settings: "作品设定", script: "本章剧本", storyboard: "分页分镜", page_prompt: "本页 Prompt", page_prompts: "每页 Prompt", images: "漫画", optimize: "AI 优化", sync: "联动更新" };
export function mdTemplate(kind: MdKind, pageNo = 1): string {
  const sections: Record<MdKind, string[]> = {
    settings: ["世界观", "画风", "人物锚点"],
    script: ["剧情", "场景与对白", "人物锚点补充"],
    storyboard: ["本页剧情", "分镜", "画面文字", "人物状态"],
    page_prompt: ["画面要求", "世界观与场景", "人物锚点", "人物锚点补充", "剧情与分镜", "画面文字", "连续性要求"],
  };
  return `# ${kind === "storyboard" || kind === "page_prompt" ? `第${pageNo}页` : mdLabels[kind]}\n\n${sections[kind].map((name) => `## ${name}\n\n${name.includes("分镜") ? "### 第1格\n\n" : ""}`).join("\n")}`;
}
export function mdReady(doc: MdDocument | undefined): doc is MdDocument { return !!doc && !doc.outOfPlan && !doc.stale && doc.issues.length === 0 && !!doc.markdown.trim(); }
export function mdStageBlock(stage: MdStage, workspace: MdWorkspace): string | null {
  if (!workspace.sourceRevisionId || !workspace.sourceContent.trim()) return "请先保存章节正文。";
  const required: MdKind[] = stage === "settings" ? [] : stage === "script" ? ["settings"] : stage === "storyboard" ? ["settings", "script"] : ["settings", "script", "storyboard"];
  const missing = required.find((kind) => !mdReady(workspace.documents.find((doc) => doc.kind === kind)));
  return missing ? `请先补齐并更新${mdLabels[missing]}。` : null;
}
