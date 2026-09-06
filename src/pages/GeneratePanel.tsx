import { Component, lazy, Suspense, useState, type ReactNode } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { FileInput, Image as ImageIcon, Library, Loader2, Sparkles, Upload, Wand2, X } from "lucide-react";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { useGenerationStore, type GenParams } from "../store/useGenerationStore";
import { usePromptlibStore } from "../store/usePromptlibStore";
import { generateImage } from "../lib/generate";
import { importRefImage } from "../lib/ipc";
import { IMAGE_MODELS } from "../lib/models";
import { ContextMenu } from "../components/ContextMenu";
import HistoryImageCard from "../components/HistoryImageCard";
import AssetPicker from "../components/AssetPicker";
import type { PromptLibraryPick } from "../components/PromptLibrary";
import OptimizePromptDialog from "../components/OptimizePromptDialog";
import { logEvent } from "../lib/logger";
import AssetDetailModal from "../components/AssetDetailModal";
import WorkflowGuide from "../components/WorkflowGuide";
import { getDocumentMeta } from "../lib/documents";
import { snapshotAsset, type SourceMaterialSnapshot } from "../lib/provenance";
import { imageHistoryMenu } from "../lib/historyMenu";
import { isCompressedAsset } from "../lib/imageCompress";

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

export default function GeneratePanel({ llmModel }: { llmModel?: string }) {
  const gen = useGenerationStore();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; asset: LibAsset } | null>(null);
  const [previewAsset, setPreviewAsset] = useState<LibAsset | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [promptSource, setPromptSource] = useState<LibAsset | null>(null);
  const [referenceSource, setReferenceSource] = useState<LibAsset | null>(null);
  const [libraryOpen, setLibraryOpen] = useState(false);
  const [libraryLoaded, setLibraryLoaded] = useState(false);
  const [optimizeOpen, setOptimizeOpen] = useState(false);
  const [promptlibId, setPromptlibId] = useState<string | null>(null);
  const [promptlibEntry, setPromptlibEntry] = useState<PromptLibraryPick["entry"] | null>(null);
  const provider = useProjectStore();
  const defaultId = provider.projects[0]?.id;
  const activeId = provider.activeId;
  const belongs = (pid?: string) => pid === activeId || (!pid && activeId === defaultId);
  const allAssets = useLibraryStore((s) => s.assets);
  const assets = allAssets.filter((a) => belongs(a.projectId) && a.asset.kind === "image");

  const isImg2Img = gen.referencePath.trim().length > 0;
  const originals = assets.filter((item) => !isCompressedAsset(item));
  const previewItem = isImg2Img
    ? originals.find((a) => a.source === "图生图")
    : originals.find((a) => a.source === "文生图") ?? originals[0] ?? assets[0];

  const canRun = gen.prompt.trim().length > 0 && !busy;

  const pickReference = async () => {
    try {
      const p = await openDialog({ multiple: false, filters: [{ name: "图片", extensions: ["png", "jpg", "jpeg", "webp"] }] });
      if (typeof p === "string") {
        const copied = await importRefImage(p);
        gen.set({ referencePath: copied });
        setReferenceSource(null);
      }
    } catch (error) {
      setError(String(error));
      logEvent("warn", "image.reference_pick_failed", { error: String(error) });
    }
  };

  const run = async () => {
    if (!canRun) return;
    setBusy(true);
    setError(null);
    try {
      const template = promptlibId ? promptlibEntry : undefined;
      const sources: SourceMaterialSnapshot[] = [
        ...(promptSource ? [snapshotAsset(promptSource, getDocumentMeta(promptSource)?.text, "提示词来源") ] : []),
        ...(referenceSource ? [snapshotAsset(referenceSource, undefined, "参考图来源")] : []),
        ...(template ? [{
          kind: "context" as const,
          label: `提示词模板 · ${template.title}`,
          source: template.id,
          text: template.title,
        }] : []),
      ];
      await generateImage({
        prompt: gen.prompt,
        referencePath: gen.referencePath,
        size: gen.size,
        quality: gen.quality,
        background: gen.background,
        model: gen.model,
      }, {
        originalInput: gen.prompt,
        generationInput: gen.prompt,
        sourceMaterials: sources.length ? sources : undefined,
        parentAssetIds: [promptSource?.asset.id, referenceSource?.asset.id].filter((id): id is string => !!id),
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const clearPromptLibrarySelection = () => {
    const cleared = emptyPromptLibrarySelection<PromptLibraryPick["entry"]>();
    setPromptlibId(cleared.id);
    setPromptlibEntry(cleared.entry);
  };

  const modelOptions = gen.model && !IMAGE_MODELS.includes(gen.model) ? [gen.model, ...IMAGE_MODELS] : IMAGE_MODELS;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* Top: model switcher */}
      <div className="flex items-center gap-2 border-b border-slate-800 bg-slate-900/40 px-6 py-2.5">
        <span className="text-xs font-medium text-slate-400">图像模型</span>
        <select
          value={gen.model}
          onChange={(e) => gen.set({ model: e.target.value })}
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
            <textarea
              className={`${inputCls} h-24 resize-y leading-snug`}
              value={gen.prompt}
              placeholder="描述你想生成的画面…"
              onChange={(e) => { gen.set({ prompt: e.target.value }); setPromptSource(null); }}
            />
          </Field>
          <div className="mt-1 flex flex-wrap gap-2">
            <button type="button" onClick={() => { setLibraryLoaded(true); setLibraryOpen(true); }} className="flex items-center gap-1 text-[10px] text-indigo-300 hover:text-white"><Library size={11} /> 模板库</button>
            <button type="button" onClick={() => setOptimizeOpen(true)} disabled={!gen.prompt.trim()} className="flex items-center gap-1 text-[10px] text-fuchsia-300 hover:text-white disabled:opacity-40"><Sparkles size={11} /> AI 优化成中文</button>
            <button type="button" onClick={() => setPickerOpen(true)} className="flex items-center gap-1 text-[10px] text-cyan-200/75 hover:text-white"><FileInput size={11} /> 从项目历史导入</button>
          </div>
          {promptlibId && (
            <div className="mt-1 text-[10px] text-slate-500">当前模板：{promptlibEntry?.title ?? promptlibId}</div>
          )}
        </div>

        <Field label="参考图（可选）" hint={`${isImg2Img ? "已在" : "加图即图生图"}`}>
          {gen.referencePath ? (
            <div className="relative overflow-hidden rounded-lg border border-slate-700 bg-slate-800">
              <img src={convertFileSrc(gen.referencePath)} alt="ref" className="h-40 w-full object-contain" />
              <div className="flex items-center justify-between gap-2 bg-slate-900/80 px-2 py-1.5">
                <span className="truncate text-[10px] text-slate-400">
                  {gen.referencePath.split(/[\\/]/).pop()}
                </span>
                <div className="flex gap-1">
                  <button type="button" onClick={pickReference} className="rounded px-2 py-1 text-[10px] text-indigo-300 hover:bg-slate-800">
                    更换
                  </button>
                  <button type="button" onClick={() => { gen.set({ referencePath: "" }); setReferenceSource(null); }} className="rounded px-1 py-1 text-slate-500 hover:text-rose-300">
                    <X size={13} />
                  </button>
                </div>
              </div>
            </div>
          ) : (
            <button
              type="button"
              onClick={pickReference}
              className="flex w-full items-center justify-center gap-1.5 rounded-lg border border-dashed border-slate-600 bg-slate-800/40 px-3 py-3 text-xs text-slate-300 transition hover:border-slate-400 hover:text-white"
            >
              <Upload size={13} />
              上传参考图
            </button>
          )}
        </Field>

        <Field label="尺寸 / 比例">
          <select className={inputCls} value={gen.size} onChange={(e) => gen.set({ size: e.target.value })}>
            {SIZES.map((s) => (<option key={s} value={s}>{s}</option>))}
          </select>
        </Field>

        <Field label="质量">
          <select className={inputCls} value={gen.quality} onChange={(e) => gen.set({ quality: e.target.value })}>
            {["high", "medium", "low"].map((q) => (<option key={q} value={q}>{q}</option>))}
          </select>
        </Field>

        <Field label="背景">
          <select className={inputCls} value={gen.background} onChange={(e) => gen.set({ background: e.target.value })}>
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
            onReference: (asset) => { gen.set({ referencePath: asset.asset.path }); setReferenceSource(asset); },
          })}
        />
      )}
      <AssetDetailModal
        asset={previewAsset}
        onClose={() => setPreviewAsset(null)}
        onLoadAsset={(asset) => { gen.load({ ...(asset.params ?? {}), ...(asset.model ? { model: asset.model } : {}) } as Partial<GenParams>); setPromptSource(asset); clearPromptLibrarySelection(); }}
        onUseReference={(asset) => { gen.set({ referencePath: asset.asset.path }); setReferenceSource(asset); }}
      />

      <AssetPicker
        open={pickerOpen}
        onClose={() => setPickerOpen(false)}
        kinds={["text", "image"]}
        onPick={(a) => {
          if (a.asset.kind === "text") {
            gen.set({ prompt: getDocumentMeta(a)?.text || String(a.params?.text ?? a.source) });
            setPromptSource(a);
            clearPromptLibrarySelection();
          } else {
            gen.set({ referencePath: a.asset.path });
            setReferenceSource(a);
          }
        }}
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
                  gen.set({ prompt: pick.prompt, ...(pick.referencePath ? { referencePath: pick.referencePath } : {}) });
                  setPromptSource(null);
                  setPromptlibId(pick.entry.id);
                  setPromptlibEntry(pick.entry);
                  if (pick.referencePath) setReferenceSource(null);
                  logEvent("info", "promptlib.applied", { id: pick.entry.id, asReference: Boolean(pick.asReference) });
                }}
              />
            </Suspense>
          </PromptLibraryErrorBoundary>
        ) : null;
      })()}

      <OptimizePromptDialog
        open={optimizeOpen}
        prompt={gen.prompt}
        guidance={promptlibId ? promptlibEntry?.guidance : undefined}
        pitfalls={promptlibId ? promptlibEntry?.pitfalls : undefined}
        llmModel={llmModel}
        onClose={() => setOptimizeOpen(false)}
        onAdopt={(prompt) => gen.set({ prompt })}
        onSaveAsTemplate={promptlibId ? (prompt) => usePromptlibStore.getState().saveOverride(promptlibId, prompt) : undefined}
      />
    </div>
  );
}
