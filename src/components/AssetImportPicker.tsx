import { useEffect, useMemo, useState } from "react";
import { FileText, Image as ImageIcon, Search, Video, X } from "lucide-react";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import {
  importEntryKind,
  importEntryLabel,
  importEntryPublishedUrl,
  importEntryPrompt,
  libraryImportEntry,
  type ImportEntry,
} from "../lib/assetImport";
import { classifyAssetCatalog, type AssetCatalogCategory } from "../lib/assetCatalog";

export type AssetImportAction = "prompt" | "merge_prompt" | "reference";
export type ReferenceImportMode = "replace" | "append";
export type LocalImageDelivery = "direct" | "hosting";

export interface AssetImportPickerProps {
  open: boolean;
  onClose: () => void;
  /** Explicit supported actions keep opening the picker side-effect free. */
  actions: readonly AssetImportAction[];
  kinds: readonly ("text" | "image" | "video")[];
  onApply: (input: { action: AssetImportAction; referenceMode: ReferenceImportMode; localImageDelivery: LocalImageDelivery; entries: ImportEntry[] }) => void | Promise<void>;
  /** Canonical comic records can be passed here without manufacturing a library asset ID. */
  entries?: readonly ImportEntry[];
  title?: string;
  strictProject?: boolean;
  /** A cross-page import request may preselect entries, but still requires the
   * user to inspect the preview and explicitly apply it. */
  initialEntries?: readonly ImportEntry[];
  initialAction?: AssetImportAction;
  /** Only video hosts provide a deliberate local-media publishing step. */
  onConfigureMediaHosting?: () => void;
  mediaHosting?: { configured: boolean; endpoint: string } | null;
}

function icon(kind: "text" | "image" | "video") {
  if (kind === "image") return <ImageIcon size={14} className="text-cyan-300" />;
  if (kind === "video") return <Video size={14} className="text-fuchsia-300" />;
  return <FileText size={14} className="text-amber-200" />;
}

function actionLabel(action: AssetImportAction): string {
  if (action === "prompt") return "提示词";
  if (action === "merge_prompt") return "合并选中提示词";
  return "参考资源";
}

function categoryLabel(value: AssetCatalogCategory): string {
  return ({ image_generation: "AI 生图", video_generation: "AI 视频", novel: "小说", comic: "小说漫画", short_drama: "短剧", upload: "本地上传", legacy: "其他历史资产" } as const)[value];
}

function groupLabel(entry: ImportEntry): string {
  if (entry.entryType === "canonical_comic") return `${entry.chapterNo ? `第 ${entry.chapterNo} 章` : "漫画工作"} · ${entry.title}`;
  const group = classifyAssetCatalog(entry.asset).group;
  return group?.label ?? `${group?.type === "generation_batch" ? "同批生图" : group?.type === "video_shots" ? "视频镜头组" : group?.type === "short_drama_workflow" ? "短剧工作流" : group?.type === "comic_chapter" ? "漫画章节" : group?.type === "upload_batch" ? "上传批次" : "资产组"} · ${entry.asset.source}`;
}

function orderingValue(entry: ImportEntry): number | undefined {
  if (entry.entryType === "canonical_comic") return entry.pageNo;
  const params = entry.asset.params ?? {};
  const comic = params.comicGeneration && typeof params.comicGeneration === "object"
    ? params.comicGeneration as Record<string, unknown>
    : undefined;
  const value = params.shotNo ?? comic?.pageNo ?? params.pageNo;
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function groupKey(entry: ImportEntry): string | undefined {
  if (entry.entryType === "canonical_comic") {
    return entry.novelChapterId ? `comic_chapter:${entry.novelWorkId ?? "work"}::${entry.novelChapterId}` : `comic_work:${entry.novelWorkId ?? "catalog"}`;
  }
  const group = classifyAssetCatalog(entry.asset).group;
  return group ? `${group.type}:${group.id}` : undefined;
}

function category(entry: ImportEntry): AssetCatalogCategory {
  return entry.entryType === "canonical_comic" ? "comic" : classifyAssetCatalog(entry.asset).category;
}

/** Preserve click order generally, but a chosen comic page/video-shot group has its authored order. */
export function stableImportOrder(entries: readonly ImportEntry[]): ImportEntry[] {
  const withIndex = entries.map((entry, index) => ({ entry, index, order: orderingValue(entry) }));
  const groups = new Set(entries.map(groupKey));
  const allHaveOrder = withIndex.length > 1 && groups.size === 1 && !groups.has(undefined) && withIndex.every((item) => item.order !== undefined);
  if (!allHaveOrder) return withIndex.map((item) => item.entry);
  return withIndex.sort((a, b) => a.order! - b.order! || a.index - b.index).map((item) => item.entry);
}

export default function AssetImportPicker({
  open,
  onClose,
  actions,
  kinds,
  onApply,
  entries,
  title = "从资产库导入",
  strictProject = true,
  initialEntries,
  initialAction,
  onConfigureMediaHosting,
  mediaHosting,
}: AssetImportPickerProps) {
  const activeProjectId = useProjectStore((state) => state.activeId);
  const libraryEntries = useLibraryStore((state) => state.assets);
  const [action, setAction] = useState<AssetImportAction>(actions[0] ?? "prompt");
  const [referenceMode, setReferenceMode] = useState<ReferenceImportMode>("replace");
  const [localImageDelivery, setLocalImageDelivery] = useState<LocalImageDelivery>("direct");
  const [selected, setSelected] = useState<ImportEntry[]>([]);
  const [query, setQuery] = useState("");
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [categoryFilter, setCategoryFilter] = useState<AssetCatalogCategory | "all">("all");
  const [groupFilter, setGroupFilter] = useState<string>("all");
  const [groupContent, setGroupContent] = useState<"all" | "page_prompt" | "image">("all");

  useEffect(() => {
    if (!open) {
      setSelected([]); setQuery(""); setError(null); setGroupFilter("all"); setCategoryFilter("all"); setGroupContent("all");
      return;
    }
    setSelected((initialEntries ?? []).filter((entry) => entry.entryType === "library_asset" ? entry.asset.projectId === activeProjectId : entry.projectId === activeProjectId));
    setAction(initialAction ?? actions[0] ?? "prompt");
    // actions is commonly an inline literal from the host page; it must not
    // reset a user's selection on every host render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, activeProjectId, initialAction, initialEntries]);

  const candidates = useMemo(() => {
    const source = entries ?? libraryEntries.map(libraryImportEntry);
    const needle = query.trim().toLowerCase();
    return source
      .filter((entry) => kinds.includes(importEntryKind(entry)))
      .filter((entry) => !strictProject || (entry.entryType === "library_asset"
        ? entry.asset.projectId === activeProjectId
        : entry.projectId === activeProjectId))
      .filter((entry) => !needle || importEntryLabel(entry).toLowerCase().includes(needle))
      .filter((entry) => categoryFilter === "all" || category(entry) === categoryFilter)
      .filter((entry) => groupFilter === "all" || groupKey(entry) === groupFilter)
      .filter((entry) => groupContent === "all" || (groupContent === "image" ? importEntryKind(entry) === "image" : entry.entryType === "canonical_comic" && entry.documentKind === "page_prompt"))
      .sort((left, right) => {
        const leftCreated = left.entryType === "library_asset" ? left.asset.createdAt : left.createdAt;
        const rightCreated = right.entryType === "library_asset" ? right.asset.createdAt : right.createdAt;
        return rightCreated - leftCreated;
      });
  }, [activeProjectId, categoryFilter, entries, groupContent, groupFilter, kinds, libraryEntries, query, strictProject]);

  const allProjectCandidates = useMemo(() => (entries ?? libraryEntries.map(libraryImportEntry))
    .filter((entry) => kinds.includes(importEntryKind(entry)))
    .filter((entry) => !strictProject || (entry.entryType === "library_asset" ? entry.asset.projectId === activeProjectId : entry.projectId === activeProjectId)), [activeProjectId, entries, kinds, libraryEntries, strictProject]);
  const availableCategories = [...new Set(allProjectCandidates.map(category))];
  const availableGroups = [...new Map(allProjectCandidates.flatMap((entry) => {
    const key = groupKey(entry);
    return key ? [[key, entry] as const] : [];
  })).entries()];

  if (!open) return null;
  const maxSelection = action === "prompt" ? 1 : Number.POSITIVE_INFINITY;
  const canApply = selected.length > 0 && (action !== "prompt" || selected.length === 1) && !applying;
  const localImageCount = action === "reference" && onConfigureMediaHosting ? selected.filter((entry) => {
    if (importEntryKind(entry) !== "image") return false;
    const path = entry.entryType === "library_asset" ? entry.asset.asset.path : entry.path;
    return !importEntryPublishedUrl(entry) && !(path && /^https:\/\//i.test(path));
  }).length : 0;
  const localVideoCount = action === "reference" && onConfigureMediaHosting ? selected.filter((entry) => {
    if (importEntryKind(entry) !== "video") return false;
    const path = entry.entryType === "library_asset" ? entry.asset.asset.path : entry.path;
    return !importEntryPublishedUrl(entry) && !(path && /^https:\/\//i.test(path));
  }).length : 0;
  const localReferenceCount = localVideoCount + (localImageDelivery === "hosting" ? localImageCount : 0);
  const toggle = (entry: ImportEntry) => {
    const id = entry.entryType === "library_asset" ? `asset:${entry.asset.asset.id}` : `canonical:${entry.sourceUri}`;
    setSelected((current) => {
      const exists = current.some((item) => (item.entryType === "library_asset" ? `asset:${item.asset.asset.id}` : `canonical:${item.sourceUri}`) === id);
      if (exists) return current.filter((item) => (item.entryType === "library_asset" ? `asset:${item.asset.asset.id}` : `canonical:${item.sourceUri}`) !== id);
      if (current.length >= maxSelection) return [entry];
      return [...current, entry];
    });
  };
  const apply = async () => {
    if (!canApply) return;
    const currentEntries = selected.filter((entry) => entry.entryType === "library_asset" ? entry.asset.projectId === activeProjectId : entry.projectId === activeProjectId);
    if (!currentEntries.length) { setError("项目已切换，旧项目的导入选择已清除。请在当前项目重新选择。"); setSelected([]); return; }
    const applicableEntries = action === "reference" ? currentEntries : currentEntries.filter((entry) => Boolean(importEntryPrompt(entry)));
    if (!applicableEntries.length) { setError("所选项目没有可用的正文或已保存提示词，未应用任何内容。"); return; }
    setApplying(true);
    setError(null);
    try {
      await onApply({ action, referenceMode, localImageDelivery, entries: stableImportOrder(applicableEntries) });
      setSelected([]);
      onClose();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setApplying(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4" role="dialog" aria-modal="true" aria-label={title}>
      <section className="flex max-h-[80vh] w-full max-w-2xl flex-col overflow-hidden rounded-2xl border border-slate-700 bg-slate-950 shadow-2xl">
        <header className="flex items-center gap-3 border-b border-slate-800 px-5 py-4">
          <div className="min-w-0 flex-1"><h2 className="text-sm font-semibold text-white">{title}</h2><p className="mt-0.5 text-[11px] text-slate-500">选择后先预览操作；应用不会自动开始生成。</p></div>
          <button type="button" onClick={onClose} className="rounded p-1 text-slate-400 hover:text-white" aria-label="关闭导入"><X size={16} /></button>
        </header>
        <div className="border-b border-slate-800 px-5 py-3">
          <div className="grid grid-cols-3 gap-2">
            {actions.map((value) => <button key={value} type="button" onClick={() => { setAction(value); setSelected([]); setError(null); }} className={`rounded-lg border px-3 py-2 text-xs ${action === value ? "border-indigo-300/50 bg-indigo-400/10 text-indigo-100" : "border-slate-700 text-slate-400 hover:text-white"}`}>{actionLabel(value)}</button>)}
          </div>
          <div className="mt-2 flex flex-wrap gap-1" aria-label="资产分类筛选">
            {(["all", ...availableCategories] as const).map((value) => <button key={value} type="button" onClick={() => { setCategoryFilter(value); setGroupFilter("all"); }} className={`rounded px-2 py-1 text-[10px] ${categoryFilter === value ? "bg-slate-700 text-white" : "text-slate-400"}`}>{value === "all" ? "全部分类" : categoryLabel(value)}</button>)}
          </div>
          {availableGroups.length > 0 && <select aria-label="资产组筛选" value={groupFilter} onChange={(event) => { setGroupFilter(event.target.value); setGroupContent("all"); }} className="mt-2 max-w-full rounded border border-slate-700 bg-slate-900 px-2 py-1 text-[10px] text-slate-300"><option value="all">所有组</option>{availableGroups.map(([key, entry]) => <option key={key} value={key}>{groupLabel(entry)}</option>)}</select>}
          {groupFilter !== "all" && <div className="mt-2 flex gap-1 text-[10px]" aria-label="资产组内容筛选"><button type="button" onClick={() => setGroupContent("all")} className={`rounded px-2 py-1 ${groupContent === "all" ? "bg-slate-700 text-white" : "text-slate-400"}`}>全部资料</button><button type="button" onClick={() => setGroupContent("page_prompt")} className={`rounded px-2 py-1 ${groupContent === "page_prompt" ? "bg-slate-700 text-white" : "text-slate-400"}`}>页面提示词</button><button type="button" onClick={() => setGroupContent("image")} className={`rounded px-2 py-1 ${groupContent === "image" ? "bg-slate-700 text-white" : "text-slate-400"}`}>图片版本</button></div>}
          {action === "reference" && <div className="mt-2 flex gap-2 text-[11px]"><button type="button" onClick={() => setReferenceMode("replace")} className={`rounded px-2 py-1 ${referenceMode === "replace" ? "bg-slate-700 text-white" : "text-slate-400"}`}>覆盖现有参考</button><button type="button" onClick={() => setReferenceMode("append")} className={`rounded px-2 py-1 ${referenceMode === "append" ? "bg-slate-700 text-white" : "text-slate-400"}`}>追加到现有参考</button></div>}
          <div className="relative mt-3"><Search size={14} className="pointer-events-none absolute left-3 top-2.5 text-slate-500" /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索当前项目资产" className="w-full rounded-lg border border-slate-700 bg-slate-900 py-2 pl-9 pr-3 text-xs text-slate-100 outline-none focus:border-indigo-400" /></div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-3">
          {candidates.length ? <div className="space-y-1">{candidates.map((entry) => {
            const key = entry.entryType === "library_asset" ? `asset:${entry.asset.asset.id}` : `canonical:${entry.sourceUri}`;
            const checked = selected.some((item) => (item.entryType === "library_asset" ? `asset:${item.asset.asset.id}` : `canonical:${item.sourceUri}`) === key);
            const kind = importEntryKind(entry);
            return <button key={key} type="button" onClick={() => toggle(entry)} className={`flex w-full items-center gap-3 rounded-lg border px-3 py-2 text-left ${checked ? "border-indigo-300/50 bg-indigo-400/10" : "border-transparent hover:border-slate-700 hover:bg-slate-900/70"}`}><span className={`flex h-4 w-4 shrink-0 items-center justify-center rounded border ${checked ? "border-indigo-300 bg-indigo-400 text-slate-950" : "border-slate-600"}`}>{checked ? "✓" : ""}</span>{icon(kind)}<span className="min-w-0 flex-1 truncate text-xs text-slate-200">{importEntryLabel(entry)}</span><span className="text-[10px] text-slate-500">{entry.entryType === "canonical_comic" ? "只读漫画资料" : kind === "text" ? "文本" : kind === "image" ? "图片" : "视频"}</span></button>;
          })}</div> : <div className="p-8 text-center text-xs text-slate-500">当前项目没有可导入的匹配资产。</div>}
        </div>
        <footer className="border-t border-slate-800 px-5 py-3"><div className="mb-2 text-[11px] text-slate-400">{action === "prompt" ? "覆盖当前提示词" : action === "merge_prompt" ? `将按已选顺序合并到当前提示词（${selected.length} 项）` : `${referenceMode === "replace" ? "覆盖" : "追加"}参考资源（${selected.length} 项）${localImageCount && localImageDelivery === "direct" ? `；${localImageCount} 张本地图片会随生成请求发送给视频服务` : ""}${localReferenceCount ? `；会经托管发送 ${localReferenceCount} 个本地媒体` : ""}`}</div>{action === "reference" && localImageCount > 0 && onConfigureMediaHosting && <div className="mb-2 flex items-center gap-2 text-[10px]"><span className="text-slate-400">本地图片</span><button type="button" onClick={() => setLocalImageDelivery("direct")} className={localImageDelivery === "direct" ? "text-cyan-200" : "text-slate-500"}>随生成请求直接发送</button><button type="button" onClick={() => setLocalImageDelivery("hosting")} className={localImageDelivery === "hosting" ? "text-cyan-200" : "text-slate-500"}>经托管转为 URL</button></div>}{action === "reference" && localReferenceCount > 0 && onConfigureMediaHosting && <div className="mb-2 flex items-center gap-2 text-[10px]"><span className={mediaHosting?.configured ? "text-cyan-200" : "text-amber-200"}>{mediaHosting?.configured ? `发送到 ${mediaHosting.endpoint}` : "尚未配置媒体托管"}</span><button type="button" onClick={onConfigureMediaHosting} className="text-cyan-200 hover:text-white">配置媒体托管</button></div>}{selected.length > 0 && <div aria-label="导入预览" className="mb-2 max-h-32 overflow-y-auto rounded border border-slate-800 bg-slate-900/50 p-2 text-[10px] text-slate-300">{stableImportOrder(selected).map((entry, index) => { const prompt = action === "reference" ? undefined : importEntryPrompt(entry); return <div key={`${index}:${importEntryLabel(entry)}`} className="border-b border-slate-800 py-1 last:border-0"><span>{index + 1}. {importEntryLabel(entry)}</span>{prompt ? <p className="mt-0.5 whitespace-pre-wrap text-slate-500">{prompt}</p> : action !== "reference" ? <p className="mt-0.5 text-amber-200">缺少可导入的已保存正文/提示词，将不会应用。</p> : null}</div>; })}</div>}{error && <div className="mb-2 text-xs text-rose-300">{error}</div>}<div className="flex justify-end gap-2"><button type="button" onClick={onClose} className="rounded-lg px-3 py-2 text-xs text-slate-400 hover:text-white">取消</button><button type="button" disabled={!canApply} onClick={() => void apply()} className="rounded-lg bg-indigo-500 px-3 py-2 text-xs font-medium text-white disabled:opacity-40">{action === "reference" && localReferenceCount ? "上传并引用" : "确认应用"}</button></div></footer>
      </section>
    </div>
  );
}
