import { Component, lazy, Suspense, useEffect, useMemo, useState, type ReactNode } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { FileInput, Image as ImageIcon, Library, Loader2, Sparkles, Upload, Wand2, X } from "lucide-react";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { useGenerationStore, type GenParams } from "../store/useGenerationStore";
import { usePromptlibStore } from "../store/usePromptlibStore";
import { generateImage } from "../lib/generate";
import { IMAGE_MODELS } from "../lib/models";
import { ContextMenu } from "../components/ContextMenu";
import HistoryImageCard from "../components/HistoryImageCard";
import AssetImportPicker from "../components/AssetImportPicker";
import type { PromptLibraryPick } from "../components/PromptLibrary";
import OptimizePromptDialog from "../components/OptimizePromptDialog";
import { logEvent } from "../lib/logger";
import AssetDetailModal from "../components/AssetDetailModal";
import WorkflowGuide from "../components/WorkflowGuide";
import type { SourceMaterialSnapshot } from "../lib/provenance";
import { imageHistoryMenu } from "../lib/historyMenu";
import { isCompressedAsset } from "../lib/imageCompress";
import { importExternalAssets } from "../lib/externalAssetImport";
import { useGenerationImportQueue } from "../store/useGenerationImportQueue";
import { comicMdCatalogList } from "../lib/comic/markdownApi";
import {
  createImportRecord,
  libraryImportEntry,
  importEntryPrompt,
  mergeImportedPrompts,
  replaceImportedPrompt,
  type ImportEntry,
} from "../lib/assetImport";

const PromptLibraryView = lazy(() => import("../components/PromptLibrary"));

export function promptLibraryMountPolicy(libraryLoaded: boolean, libraryOpen: boolean) {
  return { mounted: libraryLoaded, visible: libraryOpen };
}

export function emptyPromptLibrarySelection<T>() {
  return { id: null as string | null, entry: null as T | null };
}

export function promptLibraryBoundaryView(hasError: boolean, libraryOpen: boolean): "children" | "error" | "hidden" {
  if (!hasError) return "children";
  return libraryOpen ? "error" : "hidden";
}

class PromptLibraryErrorBoundary extends Component<{ children: ReactNode; open: boolean; onClose: () => void }, { hasError: boolean }> {
  state = { hasError: false };

  static getDerivedStateFromError(): { hasError: boolean } {
    return { hasError: true };
  }

  render() {
    const view = promptLibraryBoundaryView(this.state.hasError, this.props.open);
    if (view === "error") {
      return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4">
          <div className="max-w-sm rounded-xl border border-rose-300/20 bg-slate-900 p-5 text-center shadow-2xl">
            <div className="text-sm font-semibold text-rose-200">模板库加载失败</div>
            <p className="mt-2 text-xs leading-5 text-slate-400">模板资源未能加载，当前生成页仍可使用。请刷新页面后重试。</p>
            <div className="mt-4 flex justify-center gap-2">
              <button type="button" onClick={this.props.onClose} className="rounded-lg border border-slate-600 px-3 py-2 text-xs text-slate-300 hover:bg-slate-800">
                返回生成页
              </button>
              <button type="button" onClick={() => window.location.reload()} className="rounded-lg bg-indigo-500 px-3 py-2 text-xs font-medium text-white hover:bg-indigo-400">
                刷新后重试
              </button>
            </div>
          </div>
        </div>
      );
    }
    if (view === "hidden") return null;
    return this.props.children;
  }
}

const inputCls =
  "w-full rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-indigo-400 focus:ring-1 focus:ring-indigo-400/40";

const SIZES = [
  "1024x1024 (1:1)",
  "1024x1536 (2:3)",
  "1536x1024 (3:2)",
  "1280x720 (16:9)",
  "720x1280 (9:16)",
  "2048x2048 (2K)",
  "4096x4096 (4K)",
];

function Field({ label, children, hint }: { label: string; children: React.ReactNode; hint?: string }) {
  return (
    <label className="block">
      <div className="mb-1 flex items-center justify-between text-xs font-medium text-slate-300">
        <span>{label}</span>
        {hint && <span className="text-[10px] text-slate-500">{hint}</span>}
      </div>
      {children}
    </label>
  );
}

/** Read the live prompt for click-time use without subscribing the whole panel. */
const readGenerationPrompt = () => useGenerationStore.getState().prompt;

/**
 * Owns the prompt subscription so a keystroke re-renders only this textarea
 * instead of the whole generation panel.
 */
function PromptTextarea({ className, placeholder, onEdit }: { className: string; placeholder: string; onEdit?: () => void }) {
  const prompt = useGenerationStore((s) => s.prompt);
  const setGen = useGenerationStore((s) => s.set);
  return (
    <textarea
      className={className}
      value={prompt}
      placeholder={placeholder}
      onChange={(event) => {
        // Manual editing detaches the form from the selected template so
        // "AI 优化" and "存回模板" no longer use that template's rules.
        onEdit?.();
        setGen({ prompt: event.target.value });
      }}
    />
  );
}

export default function GeneratePanel({ llmModel }: { llmModel?: string }) {
  const references = useGenerationStore((s) => s.references);
  const referencePath = useGenerationStore((s) => s.referencePath);
  const importedSources = useGenerationStore((s) => s.importedSources);
  const model = useGenerationStore((s) => s.model);
  const size = useGenerationStore((s) => s.size);
  const quality = useGenerationStore((s) => s.quality);
  const background = useGenerationStore((s) => s.background);
  // Subscribe to a boolean rather than the prompt string: the textarea owns the
  // string subscription, so typing cannot re-render this whole panel.
  const hasPrompt = useGenerationStore((s) => s.prompt.trim().length > 0);
  const setGen = useGenerationStore((s) => s.set);
  const loadGen = useGenerationStore((s) => s.load);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; asset: LibAsset } | null>(null);
  const [previewAsset, setPreviewAsset] = useState<LibAsset | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerPreset, setPickerPreset] = useState<{ entries: ImportEntry[]; action: "prompt" | "merge_prompt" | "reference" } | null>(null);
  const [libraryOpen, setLibraryOpen] = useState(false);
  const [libraryLoaded, setLibraryLoaded] = useState(false);
  const [optimizeOpen, setOptimizeOpen] = useState(false);
  const [promptlibId, setPromptlibId] = useState<string | null>(null);
  const [promptlibEntry, setPromptlibEntry] = useState<PromptLibraryPick["entry"] | null>(null);
  const [canonicalComicEntries, setCanonicalComicEntries] = useState<ImportEntry[]>([]);
  const pendingImport = useGenerationImportQueue((state) => state.pending);
  const projects = useProjectStore((s) => s.projects);
  const activeId = useProjectStore((s) => s.activeId);
  const defaultId = projects[0]?.id;
  const belongs = (pid?: string) => pid === activeId || (!pid && activeId === defaultId);
  const allAssets = useLibraryStore((s) => s.assets);
  const assets = useMemo(
    () => allAssets.filter((a) => (a.projectId === activeId || (!a.projectId && activeId === defaultId)) && a.asset.kind === "image"),
    [allAssets, activeId, defaultId],
  );
  const importEntries = useMemo(() => [
    ...allAssets.filter((asset) => belongs(asset.projectId)).map(libraryImportEntry),
    ...canonicalComicEntries,
  ], [allAssets, canonicalComicEntries, activeId, defaultId]);

  useEffect(() => {
    const projectId = activeId ?? defaultId;
    if (!projectId) { setCanonicalComicEntries([]); return; }
    let cancelled = false;
    void comicMdCatalogList({ projectId }).then((items) => {
      if (cancelled || (useProjectStore.getState().activeId ?? useProjectStore.getState().projects[0]?.id) !== projectId) return;
      setCanonicalComicEntries(items.map((item) => ({ entryType: "canonical_comic" as const, readonly: true as const, ...item })));
    }).catch((cause) => logEvent("warn", "asset_import.comic_catalog_failed", { projectId, error: String(cause) }));
    return () => { cancelled = true; };
  }, [activeId, defaultId]);

  const activeReferences = references?.length
    ? references
    : referencePath.trim() ? [{ path: referencePath.trim(), role: "base_image" as const, sortOrder: 0 }] : [];
  const isImg2Img = activeReferences.length > 0;
  const originals = useMemo(() => assets.filter((item) => !isCompressedAsset(item)), [assets]);
  const previewItem = useMemo(() => (isImg2Img
    ? originals.find((a) => a.source === "图生图")
    : originals.find((a) => a.source === "文生图") ?? originals[0] ?? assets[0]), [isImg2Img, originals, assets]);

  const canRun = hasPrompt && !busy;

  const removeReferenceAt = (index: number) => {
    const removed = activeReferences[index];
    if (!removed) return;
    const next = activeReferences.filter((_, itemIndex) => itemIndex !== index).map((item, sortOrder) => ({ ...item, sortOrder }));
    const removedAssetId = allAssets.find((asset) => asset.asset.path === removed.path)?.asset.id;
    const nextImportedSources = (importedSources ?? []).flatMap((record) => {
      if (record.action !== "reference") return [record];
      const sourceMaterials = record.sourceMaterials.filter((material) => material.path !== removed.path);
      const assetIds = removedAssetId ? record.assetIds.filter((id) => id !== removedAssetId) : record.assetIds;
      return sourceMaterials.length || assetIds.length ? [{ ...record, sourceMaterials, assetIds }] : [];
    });
    setGen({ references: next, referencePath: next[0]?.path ?? "", importedSources: nextImportedSources });
  };

  const applyReferenceEntries = (entries: readonly ImportEntry[], mode: "replace" | "append" = "replace") => {
    if (entries.some((entry) => (entry.entryType === "library_asset" ? entry.asset.asset.kind : entry.kind) !== "image")) {
      throw new Error("生图参考资源只能选择图片；视频请在视频生成页导入。");
    }
    const usable = entries.flatMap((entry) => {
      const path = entry.entryType === "library_asset" ? entry.asset.asset.path : entry.path;
      return path ? [{ entry, path }] : [];
    });
    if (!usable.length) throw new Error("所选参考资源没有可用的受控图片路径。");
    const record = createImportRecord("reference", usable.map((item) => item.entry), "生图参考资源");
    const existing = mode === "append" ? activeReferences : [];
    setGen({
      references: [...existing.map((item) => ({ ...item })), ...usable.map((item) => ({ path: item.path, role: "base_image" as const, weight: 1 }))].map((item, index) => ({ ...item, sortOrder: index })),
      // Preserve the legacy field for existing consumers; references[] remains authoritative.
      referencePath: (existing[0] ?? usable[0]).path,
      importedSources: [...(mode === "append" ? importedSources ?? [] : (importedSources ?? []).filter((item) => item.action !== "reference")), record],
    });
  };

  const applyImport = ({ action, referenceMode, entries }: { action: "prompt" | "merge_prompt" | "reference"; referenceMode: "replace" | "append"; entries: ImportEntry[] }) => {
    if (action === "reference") {
      applyReferenceEntries(entries, referenceMode);
      return;
    }
    const usable = entries.filter((entry) => Boolean(importEntryPrompt(entry)));
    if (!usable.length) throw new Error("所选资产没有已保存的可用生成提示词；请改选实际生成资料或正文版本。");
    const excluded = entries.filter((entry) => !importEntryPrompt(entry));
    const prompt = action === "prompt" ? replaceImportedPrompt(usable) : mergeImportedPrompts(useGenerationStore.getState().prompt, usable);
    if (!prompt) throw new Error("所选资产没有已保存的可用生成提示词；请改选实际生成资料或正文版本。");
    const record = createImportRecord(action, usable, action === "prompt" ? "导入提示词" : "合并提示词");
    setGen({
      prompt,
      importedSources: action === "prompt"
        ? [...(importedSources ?? []).filter((item) => item.action === "reference"), record]
        : [...(importedSources ?? []), record],
    });
    if (excluded.length) setError(`已排除 ${excluded.length} 项没有可见正文/已保存提示词的资源：${excluded.map((entry) => entry.entryType === "library_asset" ? entry.asset.source : entry.title).join("、")}`);
    clearPromptLibrarySelection();
  };

  const importExternalReferences = async () => {
    const projectId = activeId ?? defaultId;
    if (!projectId) throw new Error("请先选择项目，再将外部图片导入资产库。");
    const chosen = await openDialog({ multiple: true, filters: [{ name: "图片", extensions: ["png", "jpg", "jpeg", "webp"] }] });
    const paths = Array.isArray(chosen) ? chosen : chosen ? [chosen] : [];
    if (!paths.length) return;
    const imported = await importExternalAssets({
      projectId,
      importEntry: "image_reference",
      files: paths.map((path) => ({ path })),
      params: { referenceRole: "image_generation" },
    });
    // Do not apply an imported item to a page whose active project changed while awaiting native I/O.
    if ((useProjectStore.getState().activeId ?? useProjectStore.getState().projects[0]?.id) !== projectId) return;
    applyReferenceEntries(imported.map(libraryImportEntry));
  };

  useEffect(() => {
    if (!pendingImport || pendingImport.target !== "image") return;
    const currentProjectId = useProjectStore.getState().activeId ?? useProjectStore.getState().projects[0]?.id;
    // An Assets page request never crosses project boundaries. It opens the
    // normal picker with a preselection; cancel keeps the form unchanged.
    if (!currentProjectId || pendingImport.projectId !== currentProjectId) {
      useGenerationImportQueue.getState().clear(pendingImport.requestId);
      return;
    }
    setPickerPreset({ entries: pendingImport.entries, action: pendingImport.action });
    setPickerOpen(true);
    useGenerationImportQueue.getState().clear(pendingImport.requestId);
    // This is an event queue; requestId makes each explicit AssetsPage action run once.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingImport?.requestId]);

  const run = async () => {
    if (!canRun) return;
    // Read the live values at click time: this panel no longer subscribes to the
    // prompt string, and the user may have typed after the last render.
    const current = useGenerationStore.getState();
    setBusy(true);
    setError(null);
    try {
      const template = promptlibId ? promptlibEntry : undefined;
      const sources: SourceMaterialSnapshot[] = [
        ...(current.importedSources ?? []).flatMap((item) => item.sourceMaterials),
        ...(template ? [{
          kind: "context" as const,
          label: `提示词模板 · ${template.title}`,
          source: template.id,
          text: template.title,
        }] : []),
      ];
      await generateImage({
        prompt: current.prompt,
        referencePath: current.referencePath,
        references: current.references,
        size: current.size,
        quality: current.quality,
        background: current.background,
        model: current.model,
        importedSources: current.importedSources,
      }, {
        originalInput: current.prompt,
        generationInput: current.prompt,
        sourceMaterials: sources.length ? sources : undefined,
        parentAssetIds: [...new Set((current.importedSources ?? []).flatMap((item) => item.assetIds))],
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const clearPromptLibrarySelection = () => {
    // Bail out when nothing is selected: this is also the prompt editor's
    // on-edit hook and must not re-render the panel on every keystroke.
    if (!promptlibId && !promptlibEntry) return;
    const cleared = emptyPromptLibrarySelection<PromptLibraryPick["entry"]>();
    setPromptlibId(cleared.id);
    setPromptlibEntry(cleared.entry);
  };

  const modelOptions = model && !IMAGE_MODELS.includes(model) ? [model, ...IMAGE_MODELS] : IMAGE_MODELS;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* Top: model switcher */}
      <div className="flex items-center gap-2 border-b border-slate-800 bg-slate-900/40 px-6 py-2.5">
        <span className="text-xs font-medium text-slate-400">图像模型</span>
        <select
          value={model}
          onChange={(e) => setGen({ model: e.target.value })}
          className="rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-1.5 text-xs text-slate-100 outline-none transition focus:border-indigo-400"
        >
          {modelOptions.map((m) => (<option key={m} value={m}>{m}</option>))}
        </select>
        <span className="ml-auto text-[11px] text-slate-500">切换后本次生成使用所选模型</span>
      </div>
      <WorkflowGuide current="整理提示词与参考图" next="生成后点击历史查看完整参数，必要时加载并修改后重做" detail="文档和图片资源可从项目历史搜索导入" />

      <div className="flex min-h-0 flex-1 gap-6 overflow-y-auto p-6">
      {/* Left: form */}
      <div className="w-[340px] shrink-0 space-y-4 rounded-2xl border border-slate-800 bg-slate-900/60 p-5">
        <div className="flex items-center gap-2 text-sm font-semibold">
          <span className="flex h-7 w-7 items-center justify-center rounded-lg bg-indigo-500">
            <ImageIcon size={15} className="text-white" />
          </span>
          {isImg2Img ? "图生图" : "文生图"}
          <span className="ml-auto text-[10px] text-slate-500">{isImg2Img ? "有参考图" : "无参考图"}</span>
        </div>

        <div>
          <Field label="提示词">
            <PromptTextarea className={`${inputCls} h-24 resize-y leading-snug`} placeholder="描述你想生成的画面…" onEdit={clearPromptLibrarySelection} />
          </Field>
          <div className="mt-1 flex flex-wrap gap-2">
            <button type="button" onClick={() => { setLibraryLoaded(true); setLibraryOpen(true); }} className="flex items-center gap-1 text-[10px] text-indigo-300 hover:text-white"><Library size={11} /> 模板库</button>
            <button type="button" onClick={() => setOptimizeOpen(true)} disabled={!hasPrompt} className="flex items-center gap-1 text-[10px] text-fuchsia-300 hover:text-white disabled:opacity-40"><Sparkles size={11} /> AI 优化成中文</button>
            <button type="button" onClick={() => setPickerOpen(true)} className="flex items-center gap-1 text-[10px] text-cyan-200/75 hover:text-white"><FileInput size={11} /> 从资产库导入</button>
          </div>
          {promptlibId && (
            <div className="mt-1 text-[10px] text-slate-500">当前模板：{promptlibEntry?.title ?? promptlibId}</div>
          )}
        </div>

        <Field label="参考图（可选）" hint={`${isImg2Img ? "已在" : "加图即图生图"}`}>
          {activeReferences.length ? (
            <>
            <div className="relative overflow-hidden rounded-lg border border-slate-700 bg-slate-800">
              <img src={convertFileSrc(activeReferences[0].path)} alt="ref" className="h-40 w-full object-contain" />
              <div className="flex items-center justify-between gap-2 bg-slate-900/80 px-2 py-1.5">
                <span className="truncate text-[10px] text-slate-400">
                  {activeReferences.length} 项参考资源 · {activeReferences[0].path.split(/[\\/]/).pop()}
                </span>
                <div className="flex gap-1">
                  <button type="button" onClick={() => setPickerOpen(true)} className="rounded px-2 py-1 text-[10px] text-indigo-300 hover:bg-slate-800">
                    更换/追加
                  </button>
                  <button type="button" onClick={() => void importExternalReferences().catch((cause) => setError(String(cause)))} className="rounded px-2 py-1 text-[10px] text-cyan-200 hover:bg-slate-800">
                    上传并入库
                  </button>
                  <button type="button" onClick={() => setGen({ referencePath: "", references: [], importedSources: (importedSources ?? []).filter((item) => item.action !== "reference") })} className="rounded px-1 py-1 text-slate-500 hover:text-rose-300">
                    <X size={13} />
                  </button>
                </div>
              </div>
            </div>
            <div aria-label="已选参考图" className="mt-2 flex max-h-24 flex-wrap gap-1 overflow-y-auto rounded-lg border border-slate-800 bg-slate-950/40 p-1.5">
              {activeReferences.map((reference, index) => <div key={`${reference.path}:${index}`} className="group relative h-14 w-14 overflow-hidden rounded border border-slate-700 bg-slate-900">
                <img src={convertFileSrc(reference.path)} alt={`参考图 ${index + 1}`} className="h-full w-full object-cover" />
                <button type="button" aria-label={`移除参考图 ${index + 1}`} onClick={() => removeReferenceAt(index)} className="absolute right-0 top-0 hidden h-5 w-5 items-center justify-center bg-slate-950/85 text-[12px] text-rose-200 group-hover:flex focus:flex">×</button>
                <span className="absolute bottom-0 left-0 bg-slate-950/75 px-1 text-[9px] text-slate-300">{index + 1}</span>
              </div>)}
            </div>
            </>
          ) : (
            <button
              type="button"
              onClick={() => void importExternalReferences().catch((cause) => setError(String(cause)))}
              className="flex w-full items-center justify-center gap-1.5 rounded-lg border border-dashed border-slate-600 bg-slate-800/40 px-3 py-3 text-xs text-slate-300 transition hover:border-slate-400 hover:text-white"
            >
              <Upload size={13} />
              上传并入库参考图
            </button>
          )}
          <button type="button" onClick={() => setPickerOpen(true)} className="mt-2 flex items-center gap-1 text-[10px] text-cyan-200/75 hover:text-white"><FileInput size={11} /> 从已有资产选择或合并参考</button>
        </Field>

        <Field label="尺寸 / 比例">
          <select className={inputCls} value={size} onChange={(e) => setGen({ size: e.target.value })}>
            {SIZES.map((s) => (<option key={s} value={s}>{s}</option>))}
          </select>
        </Field>

        <Field label="质量">
          <select className={inputCls} value={quality} onChange={(e) => setGen({ quality: e.target.value })}>
            {["high", "medium", "low"].map((q) => (<option key={q} value={q}>{q}</option>))}
          </select>
        </Field>

        <Field label="背景">
          <select className={inputCls} value={background} onChange={(e) => setGen({ background: e.target.value })}>
            {["auto", "transparent", "opaque"].map((b) => (<option key={b} value={b}>{b}</option>))}
          </select>
        </Field>

        {error && <div className="rounded-lg bg-rose-500/10 px-3 py-2 text-xs text-rose-300">{error}</div>}

        <button
          onClick={run}
          disabled={!canRun}
          className="flex w-full items-center justify-center gap-2 rounded-lg bg-indigo-500 px-3 py-2.5 text-sm font-medium text-white transition hover:bg-indigo-400 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {busy ? <Loader2 size={15} className="animate-spin" /> : <Wand2 size={15} />}
          {busy ? "生成中…" : "生成"}
        </button>
      </div>

      {/* Right: preview + history */}
      <div className="min-w-0 flex-1">
        <div className="flex h-full flex-col gap-4">
          <div className="rounded-2xl border border-slate-800 bg-slate-900/40 p-5">
            <div className="mb-2 flex items-center justify-between text-xs font-semibold uppercase tracking-wide text-slate-400">
              <span>输出预览</span>
            </div>
            <div className="flex min-h-[320px] items-center justify-center rounded-xl border border-dashed border-slate-700 bg-slate-800/30">
              {previewItem ? (
                <HistoryImageCard
                  asset={previewItem}
                  tall
                  onOpen={() => setPreviewAsset(previewItem)}
                  onMenu={(x, y) => setMenu({ x, y, asset: previewItem })}
                />
              ) : busy ? (
                <Loader2 size={24} className="animate-spin text-slate-500" />
              ) : (
                <div className="text-center text-sm text-slate-500">输入提示词，点「生成」</div>
              )}
            </div>
          </div>

          {assets.length > 0 && (
            <div className="rounded-2xl border border-slate-800 bg-slate-900/40 p-5">
              <div className="mb-2 flex items-center justify-between">
                <span className="text-xs font-semibold uppercase tracking-wide text-slate-400">历史结果</span>
                <span className="text-[10px] text-slate-600">悬停看文件信息 · 右键压缩/转格式</span>
              </div>
              <div className="grid grid-cols-3 gap-3 sm:grid-cols-4 lg:grid-cols-6">
                {originals.map((a) => (
                  <HistoryImageCard
                    key={a.asset.id}
                    asset={a}
                    onOpen={() => setPreviewAsset(a)}
                    onMenu={(x, y) => setMenu({ x, y, asset: a })}
                  />
                ))}
              </div>
            </div>
          )}
        </div>
      </div>
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={imageHistoryMenu({
            asset: menu.asset,
            onPreview: setPreviewAsset,
            onReference: (asset) => applyReferenceEntries([libraryImportEntry(asset)]),
          })}
        />
      )}
      <AssetDetailModal
        asset={previewAsset}
        onClose={() => setPreviewAsset(null)}
        onLoadAsset={(asset) => { loadGen({ ...(asset.params ?? {}), ...(asset.model ? { model: asset.model } : {}) } as Partial<GenParams>); clearPromptLibrarySelection(); }}
        onUseReference={(asset) => applyReferenceEntries([libraryImportEntry(asset)])}
      />

      <AssetImportPicker
        open={pickerOpen}
        onClose={() => { setPickerOpen(false); setPickerPreset(null); }}
        kinds={["text", "image"]}
        actions={["prompt", "merge_prompt", "reference"]}
        onApply={applyImport}
        entries={importEntries}
        initialEntries={pickerPreset?.entries}
        initialAction={pickerPreset?.action}
      />

      {(() => {
        const libraryMount = promptLibraryMountPolicy(libraryLoaded, libraryOpen);
        return libraryMount.mounted ? (
          <PromptLibraryErrorBoundary open={libraryMount.visible} onClose={() => setLibraryOpen(false)}>
            <Suspense fallback={libraryMount.visible ? <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4"><div className="rounded-xl border border-slate-700 bg-slate-900 px-4 py-3 text-xs text-slate-300">正在加载模板库…</div></div> : null}>
              <PromptLibraryView
                open={libraryMount.visible}
                onClose={() => setLibraryOpen(false)}
                llmModel={llmModel}
                onPick={(pick: PromptLibraryPick) => {
                  const importedSources = pick.referencePath && pick.referenceAsset
                    ? [createImportRecord("reference", [libraryImportEntry(pick.referenceAsset)], "提示词模板参考图")]
                    : [];
                  setGen({
                    prompt: pick.prompt,
                    ...(pick.referencePath ? { referencePath: pick.referencePath, references: [] } : {}),
                    importedSources,
                  });
                  setPromptlibId(pick.entry.id);
                  setPromptlibEntry(pick.entry);
                  logEvent("info", "promptlib.applied", { id: pick.entry.id, asReference: Boolean(pick.asReference) });
                }}
              />
            </Suspense>
          </PromptLibraryErrorBoundary>
        ) : null;
      })()}

      <OptimizePromptDialog
        open={optimizeOpen}
        getPrompt={readGenerationPrompt}
        guidance={promptlibId ? promptlibEntry?.guidance : undefined}
        pitfalls={promptlibId ? promptlibEntry?.pitfalls : undefined}
        llmModel={llmModel}
        onClose={() => setOptimizeOpen(false)}
        onAdopt={(prompt) => setGen({ prompt })}
        onSaveAsTemplate={promptlibId ? (prompt) => usePromptlibStore.getState().saveOverride(promptlibId, prompt) : undefined}
      />
    </div>
  );
}
