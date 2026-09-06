import { useEffect, useRef, useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { comicMdDocumentSave, comicMdDocumentHistory, comicMdExport, comicMdOptimize, mdLabels, mdTemplate, mdReady, type MdDocument, type MdHistory, type MdKind, type MdScope, type MdJob } from "../../lib/comic/markdownApi";
import { button, primary, field, draftKey, hasMdDraftChanges, savedDraftChanged, useLocalValue, type MdDraft } from "./mdWorkspaceState";

export default function MarkdownDocumentEditor({ scope, kind, pageNo, document, pageDocuments, missingPageNos, jobs, refresh, renderingDisabled, onRender, injection }: {
  scope: MdScope; kind: MdKind; pageNo?: number; document?: MdDocument; pageDocuments: MdDocument[];
  missingPageNos: number[]; refresh: () => Promise<void>; renderingDisabled: boolean; onRender: (documents: MdDocument[]) => Promise<void>; injection: string; jobs: MdJob[];
}) {
  const [draft, update, storageError] = useLocalValue<MdDraft | null>(draftKey(scope, kind, pageNo), null);
  const [history, setHistory] = useState<MdHistory[]>([]);
  const [selectedVersion, selectVersion] = useState<number | null>(null);
  const [historyDrafts, setHistoryDrafts] = useState<Record<number, MdDraft>>({});
  const [historyError, setHistoryError] = useState("");
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const active = useRef(true);
  useEffect(() => { active.current = true; return () => { active.current = false; }; }, []);
  const [message, setMessage] = useState("");
  const [preview, setPreview] = useState(false);
  const [exportPath, setExportPath] = useState("");
  const [optimizeAll, setOptimizeAll] = useState(false);
  const historyRequest = useRef(0);
  const interactionEpoch = useRef(0);
  const pendingOptimize = useRef<{ id: string; revision: number; epoch: number } | null>(null);
  const navigate = (revision: number | null) => { interactionEpoch.current++; selectVersion(revision); };
  const loadHistory = async () => {
    if (!document) return;
    const request = ++historyRequest.current;
    try { const entries = await comicMdDocumentHistory({ ...scope, documentId: document.id }); if (active.current && historyRequest.current === request) { setHistory(entries); setHistoryError(""); } }
    catch (cause) { if (active.current && historyRequest.current === request) setHistoryError(String(cause)); }
  };
  useEffect(() => { void loadHistory(); }, [document?.id, document?.revision]);
  const versions = [...history.filter((entry) => entry.revision !== document?.revision), ...(document ? [{ revision: document.revision, markdown: document.markdown, optimizationInstruction: document.optimizationInstruction, createdAt: document.updatedAt }] : [])].sort((a, b) => b.revision - a.revision);
  const selectedHistory = selectedVersion === null ? undefined : versions.find((entry) => entry.revision === selectedVersion);
  const currentValue: MdDraft = draft ?? { markdown: document?.markdown ?? mdTemplate(kind, pageNo), optimizationInstruction: document?.optimizationInstruction ?? "", expectedRevision: document?.revision ?? null };
  const visible: MdDraft = selectedVersion !== null && selectedHistory ? historyDrafts[selectedVersion] ?? { markdown: selectedHistory.markdown, optimizationInstruction: selectedHistory.optimizationInstruction ?? "", expectedRevision: document?.revision ?? null } : currentValue;
  const currentRef = useRef(draft); currentRef.current = draft;
  useEffect(() => {
    const pending = pendingOptimize.current;
    if (!pending) return;
    const job = jobs.find((entry) => entry.id === pending.id);
    if (!job || job.status === "running") return;
    pendingOptimize.current = null;
    if (job.status === "succeeded" && document && document.revision > pending.revision) {
      if (interactionEpoch.current === pending.epoch) selectVersion(hasMdDraftChanges(currentRef.current, document) ? document.revision : null);
      setMessage("AI 优化已完成，已保存为新版本。当前未保存草稿仍可返回。");
    } else setMessage(job.message || "本次优化未完成，文字与优化要求已保留。");
  }, [document?.revision, jobs]);
  const visibleRef = useRef(visible); visibleRef.current = visible;
  const instruction = visible.optimizationInstruction ?? "";
  const liveDirty = hasMdDraftChanges(draft, document);
  const historicalDirty = selectedVersion !== null && (selectedVersion !== document?.revision || visible.markdown !== document.markdown || instruction !== (document.optimizationInstruction ?? ""));
  const dirty = liveDirty || historicalDirty;
  const canSave = !document || document.stale || selectedVersion !== null && selectedVersion !== document.revision || visible.markdown !== document.markdown || instruction !== (document.optimizationInstruction ?? "");
  const edit = (patch: Partial<MdDraft>) => {
    interactionEpoch.current++;
    const next = { ...visible, ...patch };
    if (selectedVersion !== null) setHistoryDrafts((entries) => ({ ...entries, [selectedVersion]: next }));
    else update(!hasMdDraftChanges(next, document) ? null : next);
  };
  const persistVisible = async (acknowledgeUpdates = false): Promise<MdDocument> => {
    const changed = !document || historicalDirty || visible.markdown !== document.markdown || instruction !== (document.optimizationInstruction ?? "");
    if (!changed && !acknowledgeUpdates && document && !hasMdDraftChanges(selectedVersion === null ? draft : null, document)) return document;
    const submitted = visibleRef.current;
    const submittedLive = currentRef.current;
    const version = selectedVersion;
    const saved = await comicMdDocumentSave({ ...scope, kind, pageNo, markdown: submitted.markdown, optimizationInstruction: submitted.optimizationInstruction ?? "", expectedRevision: version === null ? submitted.expectedRevision : document?.revision ?? null, ...(acknowledgeUpdates ? { acknowledgeUpdates: true } : {}) });
    if (!active.current) return saved;
    if (version === null) {
      if (currentRef.current === submittedLive) update(null);
      else if (currentRef.current) update({ ...currentRef.current, expectedRevision: saved.revision });
    } else {
      setHistory((entries) => [{ revision: saved.revision, markdown: saved.markdown, optimizationInstruction: saved.optimizationInstruction ?? "", createdAt: saved.updatedAt }, ...entries.filter((entry) => entry.revision !== saved.revision)]);
      // The live draft is deliberately untouched while adopting a historical version.
      selectVersion(saved.revision);
      if (visibleRef.current !== submitted) setHistoryDrafts((entries) => ({ ...entries, [saved.revision]: visibleRef.current }));
    }
    await refresh();
    return saved;
  };
  const perform = async (action: () => Promise<void>) => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setMessage("");
    try { await action(); } catch (cause) { if (active.current) setMessage(String(cause)); }
    finally { busyRef.current = false; if (active.current) setBusy(false); }
  };
  const canAcknowledge = !!document?.stale && !dirty;
  const save = () => perform(async () => { const saved = await persistVisible(canAcknowledge); setMessage(saved.stale ? "文字与优化要求已保存，仍有依赖需要更新，请查看更新原因。" : saved.issues.length ? "文字与优化要求已保存，请补齐必要内容后继续。" : "文字与优化要求已保存。"); });
  const obsoleteAll = optimizeAll && !!document?.outOfPlan;
  const missingAll = optimizeAll && kind === "page_prompt" ? [...missingPageNos].sort((a, b) => a - b) : [];
  const otherDirty = optimizeAll && kind === "page_prompt" ? pageDocuments.find((doc) => !doc.outOfPlan && doc.id !== document?.id && savedDraftChanged(scope, doc)) : undefined;
  const optimize = () => perform(async () => {
    const submittedEpoch = interactionEpoch.current;
    if (renderingDisabled || !instruction.trim()) return;
    if (obsoleteAll) throw new Error("本页不在当前分镜中，请回到有效页后再优化全部页。");
    if (missingAll.length) throw new Error(`分镜仍缺少第${missingAll.join("、")}页 Prompt，请先补齐后再优化全部页。`);
    if (otherDirty) throw new Error(`第${otherDirty.pageNo}页有未保存修改或版本冲突，请先保存该页再优化全部页。`);
    const submittedInstruction = instruction;
    // Freeze all other heads before saving this editor; the saved revision replaces only this target.
    const others = optimizeAll && kind === "page_prompt" ? pageDocuments.filter((doc) => !doc.outOfPlan && doc.id !== document?.id).map((doc) => ({ documentId: doc.id, revision: doc.revision, pageNo: doc.pageNo ?? 0 })) : [];
    const saved = await persistVisible();
    if (!active.current) return;
    const targets = [...others, { documentId: saved.id, revision: saved.revision, pageNo: saved.pageNo ?? 0 }].sort((a, b) => a.pageNo - b.pageNo).map(({ documentId, revision }) => ({ documentId, revision }));
    const job = await comicMdOptimize({ ...scope, targets, instruction: submittedInstruction, ...(optimizeAll && kind === "page_prompt" ? { allPages: true } : {}) });
    pendingOptimize.current = { id: job.id, revision: saved.revision, epoch: submittedEpoch };
    await refresh(); setMessage("已提交 AI 优化。已有版本保留，结果会成为新版本。");
  });
  const navigation = versions.map((entry) => entry.revision);
  const versionIndex = navigation.indexOf(selectedVersion ?? document?.revision ?? -1);
  const copy = async () => { try { await navigator.clipboard.writeText(visible.markdown); setMessage("已复制完整 Markdown。"); } catch { setMessage("复制失败，请在编辑框中全选复制。"); } };
  return <section aria-label={`${mdLabels[kind]}编辑器`} className="space-y-3">
    <div className="flex flex-wrap items-center gap-2">
      <label className="text-sm text-slate-300">版本 <select aria-label="文档版本" className={field} disabled={busy} value={selectedVersion ?? "current"} onChange={(event) => navigate(event.target.value === "current" ? null : Number(event.target.value))}><option value="current">当前版本{hasMdDraftChanges(draft, document) ? "（含未保存草稿）" : document ? ` · 第${document.revision}版` : "（未保存）"}</option>{versions.map((entry) => <option key={entry.revision} value={entry.revision}>第 {entry.revision} 版</option>)}</select></label>
      <button className={button} disabled={busy || versionIndex < 0 || versionIndex >= navigation.length - 1} onClick={() => navigate(navigation[versionIndex + 1])}>上一版</button>
      <button className={button} disabled={busy || versionIndex <= 0} onClick={() => navigate(navigation[versionIndex - 1])}>下一版</button>
      {selectedVersion !== null && <button className={button} disabled={busy} onClick={() => navigate(null)}>返回当前草稿</button>}
      <button className={button} onClick={() => setPreview(!preview)}>{preview ? "返回编辑" : "阅读预览"}</button>
    </div>
    {historyError && <p role="alert" className="text-sm text-amber-200">历史版本暂时无法读取：{historyError}</p>}
    {selectedVersion !== null && <p className="text-sm text-indigo-200">正在查看第 {selectedVersion} 版。正文与优化要求同步切换；编辑或采用后保存为新版本，当前未保存草稿仍保留。</p>}
    {document?.outOfPlan && <p className="rounded-lg bg-slate-800/60 p-3 text-sm text-slate-300">本页不在当前分镜中。旧文字和历史仍保留，可复制或单独导出旧资料；不参与当前漫画生成与默认导出。</p>}
    {document?.stale && <div className="rounded-lg bg-amber-500/10 p-3 text-sm leading-6 text-amber-100"><p>这份文字需要联动更新。{dirty ? "保存修改不会自动确认已处理依赖；保存后请继续核对更新原因。" : "核对已保存文字后，可点击“保存更新”确认；尚未更新的上游依赖仍需先处理。"}</p><ul className="list-inside list-disc">{(document.staleReasons?.length ? document.staleReasons : ["上游内容已有变化"]).map((reason, index) => <li key={index}>{reason}</li>)}</ul></div>}
    {!!document?.issues.length && <div className="text-sm text-amber-200"><p>继续使用前，请补齐：</p><ul className="list-inside list-disc">{document.issues.map((issue, index) => <li key={index}>{issue}</li>)}</ul></div>}
    {preview ? <MarkdownPreview markdown={visible.markdown} /> : <textarea aria-label={`${mdLabels[kind]} Markdown`} spellCheck={false} className={`${field} min-h-96 w-full resize-y font-mono leading-7`} value={visible.markdown} onChange={(event) => edit({ markdown: event.target.value })} />}
    <div className="flex flex-wrap items-center gap-2">
      <button className={primary} disabled={busy || !canSave} onClick={() => void save()}>{busy ? "处理中…" : canAcknowledge ? "保存更新" : selectedVersion !== null ? "采用此版本并保存" : "保存文字"}</button>
      <button className={button} onClick={() => void copy()}>复制全文</button>
      <button className={button} disabled={!document || dirty} onClick={() => void comicMdExport({ ...scope, documentIds: [document!.id] }).then((result) => { setExportPath(result.files[0] ?? ""); setMessage(`已导出：${result.path}`); }, (cause) => setMessage(String(cause)))}>{document?.outOfPlan ? "导出旧页 MD" : "导出 MD"}</button>
      {exportPath && <button className={button} onClick={() => void revealItemInDir(exportPath).catch((cause) => setMessage(String(cause)))}>打开导出文件位置</button>}
      <button className={button} disabled={!document} onClick={() => void loadHistory()}>历史版本</button>
      {kind === "page_prompt" && <button className={primary} disabled={busy || renderingDisabled || dirty || !mdReady(document)} onClick={() => void onRender([document!])}>生成这一页漫画</button>}
    </div>
    <div className="flex flex-wrap items-end gap-2">
      <label className="min-w-52 flex-1 text-sm text-slate-300">AI 优化要求<textarea aria-label="AI 优化要求" className={`${field} mt-1 min-h-20 w-full`} value={instruction} onChange={(event) => edit({ optimizationInstruction: event.target.value })} placeholder="例如：加强人物动作和冲突，保持人物外貌与剧情一致" /></label>
      {kind === "page_prompt" && <label className="flex items-center gap-2 py-2 text-sm text-slate-300"><input type="checkbox" disabled={!!document?.outOfPlan} checked={optimizeAll} onChange={(event) => setOptimizeAll(event.target.checked)} />优化全部页</label>}
      <button className={primary} disabled={busy || renderingDisabled || !instruction.trim() || !!otherDirty || obsoleteAll || !!missingAll.length} onClick={() => void optimize()}>AI 优化</button>
    </div>
    <p className="text-sm text-slate-500">优化要求随文字版本保存。AI 优化会先保存当前编辑，再调用文本模型；可能计费，不会自动画图。{optimizeAll && kind === "page_prompt" && " 同一要求将逐页优化本章当前分镜中所有已保存页。"}</p>
    {document?.outOfPlan && kind === "page_prompt" && <p className="text-sm text-amber-200">本页不在当前分镜中，如需优化全部页，请回到当前分镜中的有效页。</p>}
    {otherDirty && <p role="alert" className="text-sm text-amber-200">第{otherDirty.pageNo}页有未保存修改或版本冲突，请先保存该页再优化全部页。</p>}
    {!!missingAll.length && <p role="alert" className="text-sm text-amber-200">分镜仍缺少第{missingAll.join("、")}页 Prompt，请先补齐后再优化全部页。</p>}
    {kind === "page_prompt" && <p className="text-sm text-slate-400">本章生图注入优先于页 Prompt 中相冲突的要求：{injection || "未设置"}。可在“漫画”中修改，所有生图入口统一应用。</p>}
    {kind === "page_prompt" && (renderingDisabled || dirty || !mdReady(document)) && <p className="text-sm text-amber-200">{document?.outOfPlan ? "本页不在当前分镜中，不能生成漫画。" : renderingDisabled ? "当前已有任务进行中，请完成后再出图。" : liveDirty && selectedVersion !== null ? "当前草稿有未保存修改，请返回当前草稿先处理。" : historicalDirty ? "请先采用此版本并保存，再生成漫画。" : dirty || !document ? "请先保存这页 Prompt 和优化要求，再生成漫画。" : document.stale ? "请先更新这页 Prompt，再生成漫画。" : "请补齐上方列出的必要内容，再生成漫画。"}</p>}
    <p className="text-sm text-slate-500">{dirty ? "当前编辑尚未用于出图；未保存的当前草稿会保留。" : document ? `已保存第 ${document.revision} 版。` : "可以填写模板或粘贴自己的 Markdown；允许保存未完成草稿。"}</p>
    {selectedVersion === null && draft && document && draft.expectedRevision !== document.revision && <p role="alert" className="text-sm text-amber-200">已有较新版本，当前草稿仍保留。请参考版本内容合并；直接保存会提示版本冲突。<button className={`${button} ml-2`} onClick={() => { const next = { ...visible, expectedRevision: document.revision }; update(hasMdDraftChanges(next, document) ? next : null); }}>保留草稿，以当前版本为基准</button></p>}
    {storageError && <p role="alert" className="text-sm text-amber-200">草稿暂时无法写入本地，请先复制全文再切换。</p>}
    {message && <p role="status" className="text-sm text-slate-300">{message}</p>}
  </section>;
}

function MarkdownPreview({ markdown }: { markdown: string }) {
  return <article aria-label="Markdown 阅读预览" className="min-h-80 space-y-2 rounded-lg border border-slate-800 bg-slate-950/30 p-6 text-sm leading-7 text-slate-200">{markdown.split("\n").map((line, index) => {
    const heading = /^(#{1,6})\s+(.+)$/.exec(line);
    if (heading) return <div key={index} role="heading" aria-level={heading[1].length} className={heading[1].length === 1 ? "pb-3 pt-4 text-xl font-semibold" : "pt-4 text-base font-semibold text-indigo-100"}>{heading[2]}</div>;
    return <p key={index} className="whitespace-pre-wrap break-words">{line || "\u00a0"}</p>;
  })}</article>;
}
