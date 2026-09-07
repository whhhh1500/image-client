import { useState } from "react";
import { mdLabels, type MdDocument, type MdImage, type MdScope, type MdWorkspace } from "../../lib/comic/markdownApi";
export const button = "rounded-lg border border-slate-700 px-3 py-2 text-sm text-slate-200 hover:bg-slate-800 disabled:cursor-not-allowed disabled:opacity-40";
export const primary = `${button} border-indigo-400/40 bg-indigo-500/20 text-indigo-100`;
export const field = "rounded-lg border border-slate-700 bg-slate-950/50 px-3 py-2 text-sm text-slate-100 outline-none focus:border-indigo-400";
export function readLocal<T>(key: string, fallback: T): T {
  try { const raw = localStorage.getItem(key); return raw === null ? fallback : JSON.parse(raw) as T; } catch { return fallback; }
}
export function useLocalValue<T>(key: string, fallback: T) {
  const [value, setValue] = useState<T>(() => readLocal(key, fallback));
  const [storageError, setStorageError] = useState(false);
  const update = (next: T) => {
    setValue(next);
    try { if (next === null) localStorage.removeItem(key); else localStorage.setItem(key, JSON.stringify(next)); setStorageError(false); } catch { setStorageError(true); }
    window.dispatchEvent(new Event("comic-md-draft-change"));
  };
  return [value, update, storageError] as const;
}
export function scopeKey(scope: MdScope) { return `${scope.projectId}:${scope.novelWorkId}:${scope.chapterId}`; }
export interface MdDraft { markdown: string; optimizationInstruction?: string; expectedRevision: number | null }
export function draftKey(scope: MdScope, kind: string, pageNo?: number | null) { return `comic-md:draft:${scopeKey(scope)}:${kind}:${pageNo ?? ""}`; }
export function hasMdDraftChanges(draft: MdDraft | null, document?: MdDocument) {
  return !!draft && (!document || draft.markdown !== document.markdown || (draft.optimizationInstruction ?? "") !== (document.optimizationInstruction ?? "") || draft.expectedRevision !== document.revision);
}
export function savedDraftChanged(scope: MdScope, document: MdDocument) {
  return hasMdDraftChanges(readLocal<MdDraft | null>(draftKey(scope, document.kind, document.pageNo), null), document);
}
export function pageBlock(scope: MdScope, documents: MdDocument[]): string | null {
  if (!documents.length) return "还没有页 Prompt，请先添加并保存第1页。";
  for (const doc of documents) {
    if (doc.outOfPlan) return `第${doc.pageNo}页不在当前分镜中，可查看旧内容，但不参与本章出图。`;
    if (savedDraftChanged(scope, doc)) return `第${doc.pageNo}页有未保存修改或版本冲突，请先保存处理。`;
    if (doc.stale) return `第${doc.pageNo}页需要更新，请先核对并保存更新。`;
    if (doc.issues.length || !doc.markdown.trim()) return `第${doc.pageNo}页内容不完整：${doc.issues.join("；") || "请补齐并保存 Prompt。"}`;
  }
  return null;
}
export type RenderSelection = "first" | "first_three" | "remaining" | "all";
export function imageMatchesDocument(image: MdImage, document: MdDocument) {
  return image.documentId === document.id && (image.contentHash && document.contentHash ? image.contentHash === document.contentHash : image.documentRevision === document.revision);
}
export function syncDraftBlock(scope: MdScope, workspace: MdWorkspace): string | null {
  const plan = workspace.syncPlan;
  if (!plan) return "联动更新计划尚未读取，请稍后重试。";
  for (const target of plan.targets) {
    const document = workspace.documents.find((doc) => doc.id === target.documentId);
    const label = target.kind === "page_prompt" ? `第${target.pageNo}页 Prompt` : mdLabels[target.kind];
    if (!document || document.revision !== target.revision) return `${label}的保存版本已变化，请刷新后再更新。`;
    if (savedDraftChanged(scope, document)) return `${label}有未保存修改或版本冲突，请先保存处理再联动更新。`;
  }
  for (const pageNo of plan.missingPageNos) {
    if (hasMdDraftChanges(readLocal<MdDraft | null>(draftKey(scope, "page_prompt", pageNo), null))) return `待补的第${pageNo}页有未保存草稿，请先保存处理再联动更新。`;
  }
  const firstAffectedPage = plan.targets
    .map((target) => target.pageNo)
    .filter((pageNo): pageNo is number => pageNo !== null)
    .concat(plan.missingPageNos)
    .sort((a, b) => a - b)[0];
  if (firstAffectedPage !== undefined) {
    const forwardDraft = workspace.documents.find((document) =>
      document.kind === "page_prompt"
      && !document.outOfPlan
      && (document.pageNo ?? 0) >= firstAffectedPage
      && savedDraftChanged(scope, document));
    if (forwardDraft) return `第${forwardDraft.pageNo}页 Prompt 可能被本次联动更新，且有未保存修改或版本冲突，请先保存处理。`;
  }
  return plan.blockedReason;
}
export function selectRenderPages(scope: MdScope, workspace: MdWorkspace, selection: RenderSelection, injection: string) {
  const prompts = workspace.documents.filter((doc) => doc.kind === "page_prompt" && !doc.outOfPlan);
  const maxPage = Math.max(0, ...prompts.map((doc) => doc.pageNo ?? 0), ...(workspace.syncPlan?.missingPageNos ?? []));
  const last = selection === "first" ? 1 : selection === "first_three" ? Math.min(3, Math.max(1, maxPage)) : Math.max(1, maxPage);
  const documents: MdDocument[] = [];
  for (let no = 1; no <= last; no++) {
    const matches = prompts.filter((doc) => doc.pageNo === no);
    if (matches.length !== 1) return { documents: [], reason: matches.length ? `第${no}页编号重复，请先处理。` : `缺少第${no}页 Prompt，请先添加并保存。` };
    const doc = matches[0];
    const blocked = pageBlock(scope, [doc]);
    if (blocked) return { documents: [], reason: blocked };
    const visualRevision = workspace.workVisualProfile?.revision ?? 0;
    const currentImage = !doc.stale && workspace.images.some((image) => image.fileAvailable !== false && imageMatchesDocument(image, doc) && (image.promptInjection ?? "") === injection && (image.visualProfileRevision ?? 0) === visualRevision);
    if (selection !== "remaining" || !currentImage) documents.push(doc);
  }
  return { documents, reason: documents.length ? pageBlock(scope, documents) : "所有页面均已有当前版本与当前注入对应的漫画。" };
}
