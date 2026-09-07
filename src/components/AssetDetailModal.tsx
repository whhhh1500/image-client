import { confirmAction } from "../lib/confirm";
import { useEffect, useMemo, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Copy, FilePenLine, Image as ImageIcon, Loader2, Save, Sparkles, Video, X } from "lucide-react";
import { llmChat, readTextAsset } from "../lib/ipc";
import { LLM_MODELS } from "../lib/models";
import {
  documentTypeLabel,
  documentChangeLabel,
  getDocumentDisplayVersion,
  getDocumentMeta,
  getDocumentVersions,
  saveDocumentVersion,
  type DocumentChangeType,
} from "../lib/documents";
import { stripThinking } from "../lib/aiOutput";
import { parseStoryboardShots, serializeStoryboard, type StoryboardShot } from "../lib/video/storyboard";
import VideoStoryboardEditor from "./video/VideoStoryboardEditor";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { logEvent } from "../lib/logger";
import { updateAssetMetadata } from "../lib/dbWrite";
import {
  historyProvenanceFromParams,
  inheritHistoryProvenance,
  legacyProvenanceFromAsset,
  type HistoryProvenance,
  type SourceMaterialSnapshot,
} from "../lib/provenance";

const inputCls =
  "w-full rounded-lg border border-slate-600/80 bg-slate-950/65 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-cyan-300/70 focus:ring-1 focus:ring-cyan-300/20";

function assetPrompt(asset: LibAsset): string {
  const params = asset.params ?? {};
  if (typeof params.prompt === "string") return params.prompt;
  const nested = params.params && typeof params.params === "object"
    ? params.params as Record<string, unknown>
    : {};
  return typeof nested.prompt === "string" ? nested.prompt : "";
}

function sourcePreview(material: SourceMaterialSnapshot) {
  if (!material.path || (material.kind !== "image" && material.kind !== "video")) return null;
  const src = /^https?:\/\//i.test(material.path) ? material.path : convertFileSrc(material.path);
  return material.kind === "video"
    ? <video src={src} controls className="mt-2 max-h-52 w-full rounded-lg bg-black object-contain" />
    : <img src={src} alt={material.label} className="mt-2 max-h-52 w-full rounded-lg bg-black/20 object-contain" />;
}

function ProvenancePanel({ asset }: { asset: LibAsset }) {
  const stored = historyProvenanceFromParams(asset.params);
  const provenance: HistoryProvenance | null = stored ?? legacyProvenanceFromAsset(asset);
  if (!provenance) {
    return (
      <div className="rounded-xl border border-amber-300/15 bg-amber-300/[0.035] px-3 py-2 text-xs text-amber-100/80">
        这是一条旧历史，当时只保存了产物，没有采集原始输入与来源资料；新生成记录会完整保存。
      </div>
    );
  }
  return (
    <details open className="rounded-xl border border-cyan-300/12 bg-cyan-300/[0.025] p-3">
      <summary className="cursor-pointer text-xs font-semibold text-cyan-100">
        原始资料与生成来源{stored ? "" : "（由旧参数兼容恢复）"}
      </summary>
      <div className="mt-3 space-y-3">
        {provenance.originalInput && (
          <div>
            <div className="mb-1 text-[10px] font-medium text-slate-500">最初输入</div>
            <pre className="max-h-72 overflow-auto whitespace-pre-wrap rounded-lg border border-white/5 bg-slate-950/55 p-3 text-xs leading-relaxed text-slate-200">{provenance.originalInput}</pre>
          </div>
        )}
        {provenance.generationInput && provenance.generationInput !== provenance.originalInput && (
          <div>
            <div className="mb-1 text-[10px] font-medium text-slate-500">本次实际提交内容</div>
            <pre className="max-h-72 overflow-auto whitespace-pre-wrap rounded-lg border border-white/5 bg-slate-950/55 p-3 text-xs leading-relaxed text-slate-300">{provenance.generationInput}</pre>
          </div>
        )}
        {provenance.sourceMaterials.length > 0 && (
          <div>
            <div className="mb-1 text-[10px] font-medium text-slate-500">引用资料 · {provenance.sourceMaterials.length}</div>
            <div className="grid gap-2 [grid-template-columns:repeat(auto-fit,minmax(240px,1fr))]">
              {provenance.sourceMaterials.map((material, index) => (
                <div key={`${material.assetId ?? material.path ?? material.label}-${index}`} className="min-w-0 rounded-lg border border-white/5 bg-slate-950/45 p-3">
                  <div className="flex items-center justify-between gap-2"><span className="truncate text-xs text-slate-200">{material.label}</span><span className="shrink-0 text-[9px] uppercase text-slate-600">{material.kind}</span></div>
                  {material.source && <div className="mt-1 text-[10px] text-slate-500">来源：{material.source}</div>}
                  {material.text && <pre className="mt-2 max-h-56 overflow-auto whitespace-pre-wrap rounded bg-black/15 p-2 text-[11px] leading-relaxed text-slate-300">{material.text}</pre>}
                  {sourcePreview(material)}
                  {material.path && <div className="mt-2 break-all text-[9px] text-slate-600">{material.path}</div>}
                </div>
              ))}
            </div>
          </div>
        )}
        {provenance.revision && (
          <div className="rounded-lg border border-violet-300/10 bg-violet-300/[0.025] px-3 py-2 text-[11px] text-slate-300">
            本版本来源：{provenance.revision.type}
            {provenance.revision.instruction ? <pre className="mt-1 whitespace-pre-wrap text-slate-400">{provenance.revision.instruction}</pre> : null}
          </div>
        )}
        {(provenance.contextSnapshot || provenance.systemInstruction) && (
          <details className="rounded-lg border border-white/5 bg-slate-950/35 p-2">
            <summary className="cursor-pointer text-[10px] text-slate-500">查看当时的项目上下文和 Agent 指令</summary>
            {provenance.contextSnapshot && <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap text-[10px] leading-relaxed text-slate-400">{provenance.contextSnapshot}</pre>}
            {provenance.systemInstruction && <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap border-t border-white/5 pt-2 text-[10px] leading-relaxed text-slate-500">{provenance.systemInstruction}</pre>}
          </details>
        )}
      </div>
    </details>
  );
}

export default function AssetDetailModal({
  asset,
  onClose,
  onUseText,
  onLoadAsset,
  onUseReference,
  onSaved,
}: {
  asset: LibAsset | null;
  onClose: () => void;
  onUseText?: (text: string, asset: LibAsset) => void;
  onLoadAsset?: (asset: LibAsset) => void;
  onUseReference?: (asset: LibAsset) => void;
  onSaved?: (asset: LibAsset) => void;
}) {
  const allAssets = useLibraryStore((state) => state.assets);
  const projectId = useProjectStore((state) => state.activeId) ?? undefined;
  const [selected, setSelected] = useState<LibAsset | null>(asset);
  const [title, setTitle] = useState("");
  const [draft, setDraft] = useState("");
  const [shots, setShots] = useState<StoryboardShot[]>([]);
  const [editing, setEditing] = useState(false);
  const [model, setModel] = useState("gemini-3.7-flash");
  const [instruction, setInstruction] = useState("优化结构、可执行性和表达，保留原有核心内容；只返回完整修改稿。");
  const [busy, setBusy] = useState<"save" | "copy" | "optimize" | null>(null);
  const [draftChangeType, setDraftChangeType] = useState<DocumentChangeType>("manual");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [mediaPrompt, setMediaPrompt] = useState("");
  const [mediaInstruction, setMediaInstruction] = useState("增强画面主体、动作、镜头、光线和氛围描述，保持原意，输出一段可直接生成的提示词。");
  const [revisionInstruction, setRevisionInstruction] = useState<string | undefined>();
  const [mediaChangeType, setMediaChangeType] = useState<"manual" | "ai_optimized">("manual");

  useEffect(() => setSelected(asset), [asset]);

  useEffect(() => {
    if (!selected || dirty) return;
    const latest = allAssets.find((item) => item.asset.id === selected.asset.id);
    if (latest && latest !== selected) setSelected(latest);
  }, [allAssets, dirty, selected]);

  useEffect(() => {
    if (asset) setNotice(null);
  }, [asset]);

  useEffect(() => {
    let cancelled = false;
    if (!selected) return;
    const meta = getDocumentMeta(selected);
    setError(null);
    setEditing(false);
    setDraftChangeType("manual");
    setRevisionInstruction(undefined);
    setMediaChangeType("manual");
    setDirty(false);
    setTitle(meta?.title ?? selected.source);
    setModel(selected.model || "gemini-3.7-flash");
    if (selected.asset.kind !== "text") {
      setDraft("");
      setShots([]);
      setModel("gemini-3.7-flash");
      setMediaPrompt(assetPrompt(selected));
      return;
    }
    const load = async () => {
      const text = meta?.text || await readTextAsset(selected.asset.path);
      if (cancelled) return;
      setDraft(text);
      setShots(meta?.shots?.length ? meta.shots : parseStoryboardShots(text));
    };
    void load().catch((cause) => !cancelled && setError(String(cause)));
    return () => { cancelled = true; };
  }, [selected]);

  const documentMeta = selected ? getDocumentMeta(selected) : null;
  const versions = useMemo(
    () => selected && documentMeta ? getDocumentVersions(selected, allAssets) : [],
    [selected, documentMeta, allAssets],
  );
  if (!selected) return null;

  const setStoryboardShots = (next: StoryboardShot[]) => {
    setShots(next);
    setDraft(serializeStoryboard(next));
    setDraftChangeType("manual");
    setRevisionInstruction(undefined);
    setDirty(true);
  };

  const attemptClose = async () => {
    if (dirty && !(await confirmAction("当前修改尚未保存，确定关闭吗？"))) return;
    onClose();
  };

  const selectVersion = async (version: LibAsset) => {
    if (dirty && !(await confirmAction("切换版本会丢失当前未保存修改，确定继续吗？"))) return;
    setNotice(null);
    setSelected(version);
  };

  const save = async (changeType: DocumentChangeType) => {
    if (!documentMeta) return;
    const content = documentMeta.documentType === "storyboard" && shots.length ? serializeStoryboard(shots) : draft;
    setBusy(changeType === "copy" ? "copy" : "save");
    setError(null);
    try {
      const saved = await saveDocumentVersion({
        title: changeType === "copy" ? `${title.replace(/\s*·\s*副本$/, "")} · 副本` : title,
        text: content,
        model,
        projectId: selected.projectId ?? projectId,
        documentType: documentMeta.documentType,
        parent: selected,
        changeType,
        agentId: documentMeta.agentId,
        revisionInstruction,
      });
      setSelected(saved);
      onSaved?.(saved);
      setEditing(false);
      setDirty(false);
      setRevisionInstruction(undefined);
      setNotice(`已保存为 v${getDocumentDisplayVersion(saved, useLibraryStore.getState().assets)}，旧版本仍保留。`);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(null);
    }
  };

  const optimize = async () => {
    if (!documentMeta || !draft.trim()) return;
    setBusy("optimize");
    setError(null);
    try {
      const storyboardRule = documentMeta.documentType === "storyboard"
        ? "必须返回完整的视频分镜 Markdown，以 # 视频分镜 开头；每镜使用 ## 第N镜，并保留所有固定的 ### 字段标题、时长、锚点、起始状态、动作过程、结束状态、承接镜头和视频 Prompt。只返回 Markdown。"
        : "只返回优化后的完整正文，不输出分析过程。";
      const system = `你是专业的${documentTypeLabel(documentMeta.documentType)}编辑器。${storyboardRule}`;
      const result = stripThinking(await llmChat(system, `【优化要求】\n${instruction}\n\n【原内容】\n${draft}`, model));
      if (documentMeta.documentType === "storyboard") {
        const parsed = parseStoryboardShots(result);
        if (!parsed.length) throw new Error("模型返回的分镜不是有效 JSON，请调整要求后重试");
        setShots(parsed);
      }
      setDraft(result);
      setEditing(true);
      setDraftChangeType("ai_optimized");
      setRevisionInstruction(instruction);
      setDirty(true);
      setNotice("优化草稿已生成。请检查内容，确认后保存为新版本。");
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(null);
    }
  };

  const optimizeMediaPrompt = async () => {
    if (!mediaPrompt.trim()) return;
    setBusy("optimize");
    setError(null);
    try {
      const mediaKind = selected.asset.kind === "video" ? "视频" : "图片";
      const result = stripThinking(await llmChat(
        `你是专业的${mediaKind}生成提示词编辑器。只返回优化后的完整提示词，不要解释。`,
        `【优化要求】\n${mediaInstruction}\n\n【原提示词】\n${mediaPrompt}`,
        model,
      ));
      setMediaPrompt(result);
      setMediaChangeType("ai_optimized");
      setDirty(true);
      setNotice("优化后的提示词已生成。可继续手改，或加载到生成页重新生成。");
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(null);
    }
  };

  const modifiedMediaAsset = (): LibAsset => ({
    ...selected,
    source: title.trim() || selected.source,
    params: {
      ...(selected.params ?? {}),
      prompt: mediaPrompt,
      provenance: inheritHistoryProvenance(
        historyProvenanceFromParams(selected.params) ?? legacyProvenanceFromAsset(selected),
        {
        revision: {
          type: mediaChangeType,
          instruction: mediaChangeType === "ai_optimized" ? mediaInstruction : undefined,
          basedOnAssetId: selected.asset.id,
        },
        },
      ),
    },
  });

  const saveMediaMetadata = async () => {
    if (selected.asset.kind === "text") return;
    setBusy("save");
    setError(null);
    try {
      const updated = modifiedMediaAsset();
      await updateAssetMetadata(updated, updated.source, updated.params ?? {});
      useLibraryStore.getState().updateAsset(updated.asset.id, updated);
      setSelected(updated);
      onSaved?.(updated);
      setDirty(false);
      setNotice("资源标题和提示词已保存。生成参数仍可加载到生成页继续修改。");
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(null);
    }
  };

  const copy = () => {
    const content = documentMeta?.documentType === "storyboard" && shots.length ? serializeStoryboard(shots) : draft;
    void navigator.clipboard?.writeText(content).catch((cause) => logEvent("warn", "clipboard.write_failed", { error: String(cause) }));
  };

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-[#020617]/85 p-4 backdrop-blur-md" onClick={attemptClose}>
      <section className="dream-dialog flex max-h-[94vh] w-full max-w-6xl overflow-hidden rounded-2xl border border-cyan-200/15 bg-slate-950/90" onClick={(event) => event.stopPropagation()}>
        {documentMeta && versions.length > 0 && (
          <aside className="hidden w-56 shrink-0 border-r border-white/5 bg-slate-950/45 p-3 md:block">
            <div className="mb-2 text-[10px] font-semibold uppercase tracking-[0.2em] text-slate-500">版本历史</div>
            <div className="max-h-[82vh] space-y-1 overflow-y-auto">
              {versions.map((versionAsset) => {
                const meta = getDocumentMeta(versionAsset)!;
                return (
                  <button
                    key={versionAsset.asset.id}
                    onClick={() => selectVersion(versionAsset)}
                    className={`w-full rounded-lg border px-2.5 py-2 text-left transition ${versionAsset.asset.id === selected.asset.id ? "border-cyan-300/35 bg-cyan-300/10" : "border-white/5 hover:border-white/15 hover:bg-white/[0.025]"}`}
                  >
                    <div className="flex items-center justify-between text-xs text-slate-200"><span>v{getDocumentDisplayVersion(versionAsset, allAssets)}</span><span className="text-[9px] text-slate-600">{documentChangeLabel(meta.changeType)}</span></div>
                    <div className="mt-1 truncate text-[10px] text-slate-500">{new Date(versionAsset.createdAt).toLocaleString()}</div>
                  </button>
                );
              })}
            </div>
          </aside>
        )}

        <div className="flex min-w-0 flex-1 flex-col">
          <header className="flex items-center gap-3 border-b border-white/5 px-5 py-4">
            <span className="flex h-9 w-9 items-center justify-center rounded-xl border border-cyan-200/15 bg-cyan-300/10 text-cyan-200">
              {selected.asset.kind === "text" ? <FilePenLine size={17} /> : selected.asset.kind === "video" ? <Video size={17} /> : <ImageIcon size={17} />}
            </span>
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-semibold text-slate-100">{documentMeta ? `${documentMeta.title} · v${getDocumentDisplayVersion(selected, allAssets)}` : selected.source}</div>
              <div className="mt-0.5 text-[10px] text-slate-500">{documentMeta ? `${documentTypeLabel(documentMeta.documentType)} · ${documentChangeLabel(documentMeta.changeType)}` : `${selected.asset.kind} · ${selected.model || "无模型信息"}`}</div>
            </div>
            <button onClick={attemptClose} className="rounded-lg p-1.5 text-slate-500 hover:bg-white/5 hover:text-white"><X size={17} /></button>
          </header>

          <div className="min-h-0 flex-1 overflow-y-auto p-5">
            {selected.asset.kind === "text" && documentMeta ? (
              <div className="space-y-4">
                <div className="grid gap-3 md:grid-cols-[1fr_220px]">
                  <label>
                    <span className="mb-1 block text-[10px] font-medium text-slate-500">标题</span>
                    <input value={title} onChange={(event) => { setTitle(event.target.value); setDraftChangeType("manual"); setRevisionInstruction(undefined); setDirty(true); }} disabled={!editing} className={inputCls} />
                  </label>
                  <label>
                    <span className="mb-1 block text-[10px] font-medium text-slate-500">优化模型</span>
                    <select value={model} onChange={(event) => setModel(event.target.value)} className={inputCls}>
                      {(model && !LLM_MODELS.includes(model) ? [model, ...LLM_MODELS] : LLM_MODELS).map((item) => <option key={item}>{item}</option>)}
                    </select>
                  </label>
                </div>

                {documentMeta.documentType === "storyboard" && shots.length ? (
                  <VideoStoryboardEditor shots={shots} editable={editing} onChange={setStoryboardShots} />
                ) : editing ? (
                  <textarea value={draft} onChange={(event) => { setDraft(event.target.value); setDraftChangeType("manual"); setRevisionInstruction(undefined); setDirty(true); }} className={`${inputCls} min-h-[48vh] resize-y font-mono text-xs leading-relaxed`} />
                ) : (
                  <pre className="min-h-[40vh] whitespace-pre-wrap rounded-xl border border-white/5 bg-slate-950/50 p-4 font-mono text-xs leading-relaxed text-slate-300">{draft}</pre>
                )}

                <div className="rounded-xl border border-violet-300/10 bg-violet-300/[0.035] p-3">
                  <div className="mb-2 flex items-center gap-2 text-xs font-medium text-violet-200"><Sparkles size={13} /> 智能优化当前草稿</div>
                  <textarea value={instruction} onChange={(event) => setInstruction(event.target.value)} className={`${inputCls} min-h-20 resize-y text-xs`} />
                  <button onClick={() => void optimize()} disabled={busy !== null || !draft.trim()} className="mt-2 flex items-center gap-1.5 rounded-lg border border-violet-300/20 bg-violet-300/10 px-3 py-1.5 text-xs text-violet-100 hover:bg-violet-300/15 disabled:opacity-40">
                    {busy === "optimize" ? <Loader2 size={13} className="animate-spin" /> : <Sparkles size={13} />} 优化为可编辑草稿
                  </button>
                </div>
              </div>
            ) : selected.asset.kind === "video" ? (
              <div className="space-y-4">
                <video src={convertFileSrc(selected.asset.path)} controls className="max-h-[68vh] w-full rounded-xl border border-white/5 bg-black object-contain" />
                <label><span className="mb-1 block text-[10px] text-slate-500">资源标题（可修改）</span><input value={title} onChange={(event) => { setTitle(event.target.value); setDirty(true); }} className={inputCls} /></label>
                <div className="grid gap-3 md:grid-cols-[1fr_220px]">
                  <label><span className="mb-1 block text-[10px] text-slate-500">视频提示词（可修改）</span><textarea value={mediaPrompt} onChange={(event) => { setMediaPrompt(event.target.value); setMediaChangeType("manual"); setDirty(true); }} className={`${inputCls} min-h-28 resize-y text-xs`} /></label>
                  <label><span className="mb-1 block text-[10px] text-slate-500">优化模型</span><select value={model} onChange={(event) => setModel(event.target.value)} className={inputCls}>{LLM_MODELS.map((item) => <option key={item}>{item}</option>)}</select></label>
                </div>
                <div className="rounded-xl border border-violet-300/10 bg-violet-300/[0.035] p-3"><textarea value={mediaInstruction} onChange={(event) => setMediaInstruction(event.target.value)} className={`${inputCls} min-h-16 resize-y text-xs`} /><button onClick={() => void optimizeMediaPrompt()} disabled={busy !== null || !mediaPrompt.trim()} className="mt-2 rounded-lg border border-violet-300/15 px-3 py-1.5 text-xs text-violet-200 disabled:opacity-40">{busy === "optimize" ? "优化中…" : "智能优化提示词"}</button></div>
                <pre className="overflow-auto rounded-xl border border-white/5 bg-slate-950/50 p-3 text-[11px] text-slate-400">{JSON.stringify(selected.params ?? {}, null, 2)}</pre>
              </div>
            ) : (
              <div className="space-y-4">
                <img src={convertFileSrc(selected.asset.path)} alt={selected.source} className="max-h-[68vh] w-full rounded-xl border border-white/5 bg-black/20 object-contain" />
                <label><span className="mb-1 block text-[10px] text-slate-500">资源标题（可修改）</span><input value={title} onChange={(event) => { setTitle(event.target.value); setDirty(true); }} className={inputCls} /></label>
                <div className="grid gap-3 md:grid-cols-[1fr_220px]">
                  <label><span className="mb-1 block text-[10px] text-slate-500">图片提示词（可修改）</span><textarea value={mediaPrompt} onChange={(event) => { setMediaPrompt(event.target.value); setMediaChangeType("manual"); setDirty(true); }} className={`${inputCls} min-h-28 resize-y text-xs`} /></label>
                  <label><span className="mb-1 block text-[10px] text-slate-500">优化模型</span><select value={model} onChange={(event) => setModel(event.target.value)} className={inputCls}>{LLM_MODELS.map((item) => <option key={item}>{item}</option>)}</select></label>
                </div>
                <div className="rounded-xl border border-violet-300/10 bg-violet-300/[0.035] p-3"><textarea value={mediaInstruction} onChange={(event) => setMediaInstruction(event.target.value)} className={`${inputCls} min-h-16 resize-y text-xs`} /><button onClick={() => void optimizeMediaPrompt()} disabled={busy !== null || !mediaPrompt.trim()} className="mt-2 rounded-lg border border-violet-300/15 px-3 py-1.5 text-xs text-violet-200 disabled:opacity-40">{busy === "optimize" ? "优化中…" : "智能优化提示词"}</button></div>
                <pre className="overflow-auto rounded-xl border border-white/5 bg-slate-950/50 p-3 text-[11px] text-slate-400">{JSON.stringify(selected.params ?? {}, null, 2)}</pre>
              </div>
            )}
            <div className="mt-4"><ProvenancePanel asset={selected} /></div>
            {notice && <div className="mt-3 rounded-lg border border-cyan-300/15 bg-cyan-300/8 px-3 py-2 text-xs text-cyan-100">{notice}</div>}
            {error && <div className="mt-3 rounded-lg border border-rose-400/15 bg-rose-400/8 px-3 py-2 text-xs text-rose-200">{error}</div>}
          </div>

          <footer className="flex flex-wrap items-center justify-between gap-2 border-t border-white/5 px-5 py-3">
            <div className="text-[10px] text-slate-600">
              {documentMeta ? "修改不会覆盖旧内容；保存后生成一个可回溯的新版本。" : "加载参数后可在生成页修改并重新生成。"}
            </div>
            <div className="flex flex-wrap justify-end gap-2">
              {documentMeta ? (
                <>
                  <button onClick={copy} className="rounded-lg border border-white/10 px-3 py-1.5 text-xs text-slate-300 hover:bg-white/5"><Copy size={12} className="mr-1 inline" />复制</button>
                  {onUseText && <button disabled={!draft.trim()} onClick={async () => { if (dirty && !(await confirmAction("当前修改未保存，仍然用于下一步并关闭吗？"))) return; onUseText(draft, selected); onClose(); }} className="rounded-lg border border-cyan-300/15 px-3 py-1.5 text-xs text-cyan-200 hover:bg-cyan-300/5 disabled:cursor-not-allowed disabled:opacity-40">用于下一步</button>}
                  <button onClick={() => { setEditing((value) => !value); setDraftChangeType("manual"); setRevisionInstruction(undefined); }} className="rounded-lg border border-white/10 px-3 py-1.5 text-xs text-slate-200 hover:bg-white/5"><FilePenLine size={12} className="mr-1 inline" />{editing ? "预览排版" : "手动修改"}</button>
                  <button onClick={() => void save("copy")} disabled={busy !== null} className="rounded-lg border border-violet-300/15 px-3 py-1.5 text-xs text-violet-200 hover:bg-violet-300/5 disabled:opacity-40">{busy === "copy" ? "保存中…" : "保存副本"}</button>
                  <button onClick={() => void save(draftChangeType)} disabled={busy !== null || !draft.trim() || !dirty} title={dirty ? "保存当前修改" : "内容尚未修改"} className="rounded-lg border border-cyan-300/20 bg-cyan-300/10 px-3 py-1.5 text-xs font-medium text-cyan-100 hover:bg-cyan-300/15 disabled:opacity-40"><Save size={12} className="mr-1 inline" />保存新版本</button>
                </>
              ) : (
                <>
                  {selected.asset.kind === "image" && onUseReference && <button onClick={() => { onUseReference(selected); onClose(); }} className="rounded-lg border border-cyan-300/20 px-3 py-1.5 text-xs text-cyan-200">设为参考图</button>}
                  <button onClick={() => void saveMediaMetadata()} disabled={busy !== null || !dirty} className="rounded-lg border border-white/10 px-3 py-1.5 text-xs text-slate-200 disabled:opacity-40">{busy === "save" ? "保存中…" : "保存资源信息"}</button>
                  {onLoadAsset && <button onClick={() => { onLoadAsset(modifiedMediaAsset()); onClose(); }} className="rounded-lg border border-cyan-300/20 bg-cyan-300/10 px-3 py-1.5 text-xs text-cyan-100">加载修改参数并继续</button>}
                </>
              )}
            </div>
          </footer>
        </div>
      </section>
    </div>
  );
}
