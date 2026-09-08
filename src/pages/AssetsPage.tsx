import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { BookOpen, FileText, Image as ImageIcon, RotateCcw, Search, Upload, X } from "lucide-react";
import { useLibraryStore, type LibAsset, type TaskRecord } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { ContextMenu, menuIcons } from "../components/ContextMenu";
import HistoryImageCard from "../components/HistoryImageCard";
import { imageHistoryMenu } from "../lib/historyMenu";
import AssetDetailModal from "../components/AssetDetailModal";
import { generateImage } from "../lib/generate";
import { generateVideo } from "../lib/generateVideo";
import type { GenParams } from "../store/useGenerationStore";
import type { VideoParams } from "../store/useVideoStore";
import { logEvent } from "../lib/logger";
import { getDocumentMeta } from "../lib/documents";
import { refreshLibraryHistory } from "../lib/dbWrite";
import NovelAssetManager from "../components/novel/NovelAssetManager";
import { novelWorkList, type NovelWork } from "../lib/novel/api";
import { assetMatchesCatalogCategory, classifyAssetCatalog, groupCatalogAssets, type AssetCatalogCategory, type AssetCatalogGroupBucket, type ComicWorkspaceCatalogEntry } from "../lib/assetCatalog";
import { comicMdCatalogList, type ComicCatalogEntry } from "../lib/comic/markdownApi";
import { importExternalAssets } from "../lib/externalAssetImport";
import { importEntryKind, libraryImportEntry, type ImportEntry } from "../lib/assetImport";

const tabs: Array<{ id: AssetCatalogCategory | "all"; label: string }> = [
  { id: "all", label: "全部" }, { id: "image_generation", label: "生图" }, { id: "video_generation", label: "视频" },
  { id: "novel", label: "小说" }, { id: "comic", label: "漫画" }, { id: "short_drama", label: "短剧" },
  { id: "upload", label: "外部上传" }, { id: "legacy", label: "其他历史" },
];
type MediaFilter = "all" | "text" | "image" | "video";
type ScopeFilter = "project" | "unassigned";
type ImportTarget = "image" | "video";
type ImportAction = "prompt" | "merge_prompt" | "reference";
type ImportActions = Partial<Record<ImportTarget, ImportAction>>;

function matchesSearch(asset: LibAsset, query: string) {
  const term = query.trim().toLowerCase();
  if (!term) return true;
  const document = getDocumentMeta(asset);
  return [asset.source, asset.model, document?.title, document?.text, asset.params?.prompt].filter(Boolean).some((value) => String(value).toLowerCase().includes(term));
}
function groupLabel(bucket: AssetCatalogGroupBucket) {
  if (bucket.group?.label) return bucket.group.label;
  const labels: Record<string, string> = { generation_batch: "生图批次", video_shots: "视频镜头组", short_drama_workflow: "短剧工作区", novel_chapter: "小说章节", comic_chapter: "漫画章节", comic_work: "漫画作品", upload_batch: "外部上传批次" };
  return bucket.group ? `${labels[bucket.group.type] ?? "资源组"} · ${bucket.assets.length} 项` : `普通资源 · ${bucket.assets.length} 项`;
}
function taskCategory(task: TaskRecord): AssetCatalogCategory {
  const pseudo: LibAsset = { asset: { id: task.id, kind: task.kind ?? "image", path: "" }, source: task.label, projectId: task.projectId, params: task.params, createdAt: task.createdAt };
  const category = classifyAssetCatalog(pseudo).category;
  return category === "legacy" ? task.kind === "video" ? "video_generation" : "image_generation" : category;
}
function comicWorkspaceEntry(entry: ComicCatalogEntry): ComicWorkspaceCatalogEntry {
  const group = entry.novelChapterId
    ? { type: "comic_chapter" as const, id: `${entry.novelWorkId}::${entry.novelChapterId}`, label: entry.chapterTitle ? `第${entry.chapterNo ?? ""}章 · ${entry.chapterTitle}` : entry.chapterNo ? `第${entry.chapterNo}章` : "漫画章节" }
    : { type: "comic_work" as const, id: entry.novelWorkId, label: "漫画作品" };
  return { entryType: "comic_workspace", readonly: true, sourceUri: entry.sourceUri, projectId: entry.projectId, kind: entry.kind, title: entry.title, ...(entry.text ? { text: entry.text } : {}), ...(entry.sourcePrompt ? { sourcePrompt: entry.sourcePrompt } : {}), ...(entry.effectivePrompt ? { effectivePrompt: entry.effectivePrompt } : {}), promptSnapshotComplete: entry.promptSnapshotComplete, ...(entry.path ? { path: entry.path } : {}), novelWorkId: entry.novelWorkId, ...(entry.novelChapterId ? { novelChapterId: entry.novelChapterId } : {}), ...(entry.chapterNo !== undefined ? { chapterNo: entry.chapterNo } : {}), ...(entry.chapterTitle ? { chapterTitle: entry.chapterTitle } : {}), ...(entry.documentId ? { documentId: entry.documentId } : {}), ...(entry.documentRevision !== undefined ? { documentRevision: entry.documentRevision } : {}), ...(entry.documentKind ? { documentKind: entry.documentKind } : {}), ...(entry.pageNo !== undefined ? { pageNo: entry.pageNo } : {}), createdAt: entry.createdAt, stale: entry.stale, classification: { category: "comic", origin: "workspace", group, isLegacy: false }, canDelete: false };
}
function comicImportEntry(entry: ComicWorkspaceCatalogEntry): ImportEntry {
  return { entryType: "canonical_comic", readonly: true, sourceUri: entry.sourceUri, projectId: entry.projectId, kind: entry.kind, title: entry.title, ...(entry.kind === "text" ? { text: entry.sourcePrompt ?? entry.text } : {}), ...(entry.kind === "image" && entry.promptSnapshotComplete && entry.effectivePrompt ? { effectivePrompt: entry.effectivePrompt } : {}), ...(entry.path ? { path: entry.path } : {}), novelWorkId: entry.novelWorkId, ...(entry.novelChapterId ? { novelChapterId: entry.novelChapterId } : {}), ...(entry.documentKind ? { documentKind: entry.documentKind } : {}), ...(entry.documentId ? { documentId: entry.documentId } : {}), ...(entry.documentRevision !== undefined ? { documentRevision: entry.documentRevision } : {}), ...(entry.chapterNo !== undefined ? { chapterNo: entry.chapterNo } : {}), ...(entry.pageNo !== undefined ? { pageNo: entry.pageNo } : {}), createdAt: entry.createdAt };
}
function suggestedAction(entry: ImportEntry, target: ImportTarget): ImportAction {
  const kind = entry.entryType === "library_asset" ? entry.asset.asset.kind : entry.kind;
  return target === "image" ? kind === "image" ? "reference" : "prompt" : kind === "text" ? "prompt" : "reference";
}
function comicGroupImport(entries: ComicWorkspaceCatalogEntry[]): { entries: ImportEntry[]; action: ImportAction } {
  const pagePrompts = entries
    .filter((entry) => entry.kind === "text" && entry.documentKind === "page_prompt")
    .map(comicImportEntry);
  if (pagePrompts.length) return { entries: pagePrompts, action: "merge_prompt" };
  const textDocuments = entries.filter((entry) => entry.kind === "text").map(comicImportEntry);
  if (textDocuments.length) return { entries: textDocuments, action: "merge_prompt" };
  return { entries: entries.filter((entry) => entry.kind === "image").map(comicImportEntry), action: "reference" };
}
function uploadReferenceEntries(entries: ImportEntry[], target: ImportTarget): ImportEntry[] {
  const kind = target === "image" ? "image" : "video";
  return entries.filter((entry) => importEntryKind(entry) === kind);
}

export default function AssetsPage({ onQueueImport }: { onQueueImport: (entries: ImportEntry[], target: ImportTarget, action: ImportAction) => void }) {
  const allAssets = useLibraryStore((state) => state.assets);
  const allTasks = useLibraryStore((state) => state.tasks);
  const activeId = useProjectStore((state) => state.activeId);
  const [tab, setTab] = useState<AssetCatalogCategory | "all">("all");
  const [media, setMedia] = useState<MediaFilter>("all");
  const [scope, setScope] = useState<ScopeFilter>("project");
  const [query, setQuery] = useState("");
  const [menu, setMenu] = useState<{ x: number; y: number; asset: LibAsset } | null>(null);
  const [preview, setPreview] = useState<LibAsset | null>(null);
  const [comicPreview, setComicPreview] = useState<ComicWorkspaceCatalogEntry | null>(null);
  const [retrying, setRetrying] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [refreshMessage, setRefreshMessage] = useState<string | null>(null);
  const [novelSource, setNovelSource] = useState<{ projectId: string; works: NovelWork[] }>();
  const [novelError, setNovelError] = useState<{ projectId: string; message: string }>();
  const [novelManagerOpen, setNovelManagerOpen] = useState(false);
  const [focusedNovelWorkId, setFocusedNovelWorkId] = useState<string | undefined>();
  const [comicSource, setComicSource] = useState<{ projectId: string; entries: ComicWorkspaceCatalogEntry[] }>();
  const [comicError, setComicError] = useState<{ projectId: string; message: string }>();
  const [uploading, setUploading] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const sourceRequest = useRef(0);

  const loadProjectSources = useCallback(async (projectId: string) => {
    const requestId = ++sourceRequest.current;
    setNovelSource(undefined); setComicSource(undefined); setNovelError(undefined); setComicError(undefined);
    const [novels, comics] = await Promise.allSettled([novelWorkList(projectId), comicMdCatalogList({ projectId })]);
    if (sourceRequest.current !== requestId || useProjectStore.getState().activeId !== projectId) return { novels: 0, comics: 0, current: false, failed: false };
    if (novels.status === "fulfilled") setNovelSource({ projectId, works: novels.value }); else setNovelError({ projectId, message: String(novels.reason) });
    if (comics.status === "fulfilled") setComicSource({ projectId, entries: comics.value.map(comicWorkspaceEntry) }); else setComicError({ projectId, message: String(comics.reason) });
    return { novels: novels.status === "fulfilled" ? novels.value.length : 0, comics: comics.status === "fulfilled" ? comics.value.length : 0, current: true, failed: novels.status === "rejected" || comics.status === "rejected" };
  }, []);
  useEffect(() => {
    setMenu(null); setPreview(null); setComicPreview(null);
    if (scope !== "project" || !activeId) {
      sourceRequest.current += 1;
      setNovelSource(undefined); setComicSource(undefined); setNovelError(undefined); setComicError(undefined);
      return;
    }
    void loadProjectSources(activeId);
  }, [activeId, loadProjectSources, scope]);

  const currentNovelWorks = novelSource?.projectId === activeId ? novelSource.works : undefined;
  const currentComicEntries = comicSource?.projectId === activeId ? comicSource.entries : undefined;
  const currentNovelError = novelError?.projectId === activeId ? novelError.message : undefined;
  const currentComicError = comicError?.projectId === activeId ? comicError.message : undefined;
  const scopedAssets = useMemo(() => allAssets.filter((asset) => scope === "unassigned" ? !asset.projectId : Boolean(activeId) && asset.projectId === activeId), [activeId, allAssets, scope]);
  const assets = useMemo(() => scopedAssets.filter((asset) => classifyAssetCatalog(asset).category !== "novel").filter((asset) => assetMatchesCatalogCategory(asset, tab)).filter((asset) => media === "all" || asset.asset.kind === media).filter((asset) => matchesSearch(asset, query)).sort((left, right) => right.createdAt - left.createdAt), [media, query, scopedAssets, tab]);
  const groups = useMemo(() => groupCatalogAssets(assets), [assets]);
  const tasks = useMemo(() => allTasks.filter((task) => scope === "unassigned" ? !task.projectId : Boolean(activeId) && task.projectId === activeId).filter((task) => tab === "all" || taskCategory(task) === tab).filter((task) => media === "all" || task.kind === media || (!task.kind && media === "image")).filter((task) => !query.trim() || [task.label, task.model, task.params?.prompt].filter(Boolean).some((value) => String(value).toLowerCase().includes(query.trim().toLowerCase()))).sort((left, right) => right.createdAt - left.createdAt), [activeId, allTasks, media, query, scope, tab]);
  const visibleNovelWorks = useMemo(() => (currentNovelWorks ?? []).filter(() => media === "all" || media === "text").filter((work) => !query.trim() || work.title.toLowerCase().includes(query.trim().toLowerCase())), [currentNovelWorks, media, query]);
  const comicEntries = useMemo(() => (currentComicEntries ?? []).filter((entry) => media === "all" || entry.kind === media).filter((entry) => !query.trim() || [entry.title, entry.text, entry.effectivePrompt, entry.chapterTitle, entry.documentKind].filter(Boolean).some((value) => String(value).toLowerCase().includes(query.trim().toLowerCase()))).sort((left, right) => (left.classification.group?.id ?? "").localeCompare(right.classification.group?.id ?? "") || (left.pageNo ?? 0) - (right.pageNo ?? 0) || right.createdAt - left.createdAt), [currentComicEntries, media, query]);
  const comicGroups = useMemo(() => {
    const grouped = new Map<string, { label: string; entries: ComicWorkspaceCatalogEntry[] }>();
    const workTitles = new Map((currentNovelWorks ?? []).map((work) => [work.id, work.title]));
    for (const entry of comicEntries) {
      const id = entry.classification.group?.id ?? entry.sourceUri;
      const fallbackLabel = entry.classification.group?.label ?? "漫画作品";
      const workTitle = workTitles.get(entry.novelWorkId);
      const label = workTitle ? `${workTitle} · ${fallbackLabel}` : fallbackLabel;
      const group = grouped.get(id) ?? { label, entries: [] };
      group.entries.push(entry);
      grouped.set(id, group);
    }
    return [...grouped.values()];
  }, [comicEntries, currentNovelWorks]);
  const showNovel = scope === "project" && Boolean(activeId) && (tab === "all" || tab === "novel");
  const showComic = scope === "project" && Boolean(activeId) && (tab === "all" || tab === "comic");
  const queueEntries = (entries: ImportEntry[], target: ImportTarget, action?: ImportAction) => { if (entries.length) onQueueImport(entries, target, action ?? suggestedAction(entries[0], target)); };
  const importButtons = (entries: ImportEntry[], className = "", actions?: ImportAction | ImportActions, entriesForTarget?: (target: ImportTarget) => ImportEntry[]) => {
    const actionFor = (target: ImportTarget) => typeof actions === "string" ? actions : actions?.[target];
    return <div className={`mt-2 flex flex-wrap gap-1.5 ${className}`}><button type="button" onClick={(event) => { event.stopPropagation(); queueEntries(entriesForTarget?.("image") ?? entries, "image", actionFor("image")); }} className="rounded border border-cyan-300/20 px-2 py-1 text-[10px] text-cyan-100 hover:bg-cyan-300/10">用于生图</button><button type="button" onClick={(event) => { event.stopPropagation(); queueEntries(entriesForTarget?.("video") ?? entries, "video", actionFor("video")); }} className="rounded border border-fuchsia-300/20 px-2 py-1 text-[10px] text-fuchsia-100 hover:bg-fuchsia-300/10">用于视频</button></div>;
  };
  const retry = async (task: TaskRecord) => { if (!task.params || retrying) return; setRetrying(task.id); try { if (task.kind === "video") await generateVideo(task.params as unknown as VideoParams); else await generateImage(task.params as unknown as GenParams); } catch (error) { logEvent("warn", "task.retry_failed", { taskId: task.id, error: String(error) }); } finally { setRetrying(null); } };
  const refresh = async () => { if (refreshing) return; setRefreshing(true); setRefreshMessage(null); try { const [history, sources] = await Promise.all([refreshLibraryHistory("asset_center_refresh"), scope === "project" && activeId ? loadProjectSources(activeId) : Promise.resolve({ novels: 0, comics: 0, current: true, failed: false })]); setRefreshMessage(sources.current ? sources.failed ? `历史已刷新；小说或漫画资料读取失败` : `已刷新 · ${history.assets.length} 个资源、${sources.novels} 部小说、${sources.comics} 条漫画资料` : `已刷新 · ${history.assets.length} 个资源`); } catch (error) { setRefreshMessage(`刷新失败：${String(error)}`); } finally { setRefreshing(false); } };
  const count = (category: AssetCatalogCategory | "all") => scopedAssets.filter((asset) => classifyAssetCatalog(asset).category !== "novel" && assetMatchesCatalogCategory(asset, category)).length + (scope === "project" && (category === "all" || category === "novel") ? currentNovelWorks?.length ?? 0 : 0) + (scope === "project" && (category === "all" || category === "comic") ? currentComicEntries?.length ?? 0 : 0);
  const openFolder = (asset: LibAsset) => void revealItemInDir(asset.asset.path).catch((error) => logEvent("warn", "asset.reveal_failed", { error: String(error) }));
  const uploadFiles = async () => { if (!activeId || uploading) return; setUploadError(null); const selected = await openDialog({ multiple: true, directory: false, filters: [{ name: "图片或视频", extensions: ["png", "jpg", "jpeg", "webp", "gif", "mp4", "webm", "mov"] }] }); const paths = Array.isArray(selected) ? selected : selected ? [selected] : []; if (!paths.length) return; setUploading(true); try { await importExternalAssets({ projectId: activeId, importEntry: "asset_library_upload", files: paths.map((path) => ({ path })) }); await refresh(); } catch (error) { setUploadError(String(error)); } finally { setUploading(false); } };

  return <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
    <div className="flex flex-wrap items-center gap-2 border-b border-white/5 bg-slate-950/25 px-6 py-3">{tabs.map((item) => <button key={item.id} type="button" onClick={() => setTab(item.id)} className={`rounded-lg border px-3 py-1.5 text-xs transition ${tab === item.id ? "border-cyan-300/25 bg-cyan-300/10 text-cyan-100" : "border-white/5 text-slate-500 hover:border-white/15 hover:text-slate-300"}`}>{item.label}<span className="ml-1.5 text-[9px] opacity-60">{count(item.id)}</span></button>)}<label className="relative ml-auto min-w-56 flex-1 sm:max-w-sm"><Search size={13} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-600" /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索标题、正文、模型或提示词" className="w-full rounded-lg border border-white/8 bg-slate-950/60 py-2 pl-8 pr-3 text-xs text-slate-200 outline-none focus:border-cyan-300/30" /></label><button type="button" onClick={() => void uploadFiles()} disabled={!activeId || uploading} className="flex items-center gap-1.5 rounded-lg border border-white/8 px-3 py-2 text-xs text-slate-400 disabled:opacity-50"><Upload size={13} />{uploading ? "上传中" : "上传图片/视频"}</button><button type="button" onClick={() => void refresh()} disabled={refreshing} className="flex items-center gap-1.5 rounded-lg border border-white/8 px-3 py-2 text-xs text-slate-400 disabled:opacity-50"><RotateCcw size={13} className={refreshing ? "animate-spin" : ""} />{refreshing ? "刷新中" : "刷新全部"}</button></div>
    <div className="flex flex-wrap items-center gap-3 border-b border-white/5 px-6 py-2 text-[11px]"><span className="text-slate-500">范围</span><button type="button" onClick={() => setScope("project")} className={scope === "project" ? "text-cyan-100" : "text-slate-500"}>当前项目</button><button type="button" onClick={() => setScope("unassigned")} className={scope === "unassigned" ? "text-cyan-100" : "text-slate-500"}>未归属资源</button><span className="ml-2 text-slate-500">媒体</span>{(["all", "text", "image", "video"] as MediaFilter[]).map((kind) => <button key={kind} type="button" onClick={() => setMedia(kind)} className={media === kind ? "text-cyan-100" : "text-slate-500"}>{kind === "all" ? "全部" : kind === "text" ? "文本" : kind === "image" ? "图片" : "视频"}</button>)}{refreshMessage && <span className="ml-auto text-slate-500">{refreshMessage}</span>}</div>
    <div className="min-h-0 flex-1 space-y-6 overflow-y-auto p-6">{uploadError && <p role="alert" className="text-xs text-rose-200">上传失败：{uploadError}</p>}
      {showNovel && <section className="dream-panel rounded-2xl border border-white/6 p-4"><div className="flex flex-wrap items-center justify-between gap-3"><div><h2 className="flex items-center gap-2 text-sm font-semibold text-slate-100"><BookOpen size={15} /> 小说资料</h2><p className="mt-1 text-xs text-slate-500">按作品查看小说，进入管理可编辑章节和原文版本。</p></div><button type="button" onClick={() => { setFocusedNovelWorkId(undefined); setNovelManagerOpen(true); }} className="rounded-lg border border-cyan-300/15 px-3 py-1.5 text-xs text-cyan-100">管理小说和章节</button></div>{currentNovelError && <p role="alert" className="mt-3 text-xs text-rose-200">读取小说失败：{currentNovelError}</p>}{currentNovelWorks === undefined && !currentNovelError ? <p className="mt-4 text-xs text-slate-500">正在读取小说资料…</p> : currentNovelWorks ? <><div className="mt-4 grid gap-2 sm:grid-cols-2 xl:grid-cols-3">{visibleNovelWorks.map((work) => <button key={work.id} type="button" onClick={() => { setFocusedNovelWorkId(work.id); setNovelManagerOpen(true); }} className="rounded-xl border border-white/8 bg-slate-950/30 p-3 text-left hover:border-cyan-300/20"><div className="truncate text-sm text-slate-200">{work.title}</div><div className="mt-1 text-[10px] text-slate-500">{typeof work.chapterCount === "number" ? `${work.chapterCount} 章` : "查看章节"} · {work.status === "archived" ? "已归档" : "进行中"}</div></button>)}</div>{!visibleNovelWorks.length && !currentNovelError && <p className="mt-4 text-xs text-slate-500">{currentNovelWorks.length ? "没有匹配的小说资料。" : "当前项目还没有小说。可从管理入口创建第一章。"}</p>}</> : null}</section>}
      {showComic && <section><h2 className="flex items-center gap-2 text-sm font-semibold text-slate-100"><ImageIcon size={15} /> 漫画资料</h2><p className="mt-1 text-xs text-slate-500">按作品、章节和页码排列；整组优先预选页面提示词，无页面提示词时才回退到文档或图片参考。</p>{currentComicError && <p role="alert" className="mt-3 text-xs text-rose-200">读取漫画资料失败：{currentComicError}</p>}{currentComicEntries === undefined && !currentComicError ? <p className="mt-3 text-xs text-slate-500">正在读取漫画资料…</p> : comicGroups.map((group) => <div key={group.entries[0]?.sourceUri ?? group.label} className="mt-4"><div className="mb-2 flex flex-wrap items-center justify-between gap-2"><h3 className="text-xs font-medium text-fuchsia-100">{group.label} · {group.entries.length} 项</h3>{(() => { const preset = comicGroupImport(group.entries); return importButtons(preset.entries, "mt-0", preset.action); })()}</div><div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(230px,1fr))]">{group.entries.map((entry) => <article key={entry.sourceUri} className="overflow-hidden rounded-xl border border-fuchsia-300/15 bg-fuchsia-300/[0.03]"><button type="button" onClick={() => setComicPreview(entry)} className="w-full text-left hover:text-white">{entry.kind === "image" ? entry.path ? <img src={convertFileSrc(entry.path)} alt={entry.title} className="aspect-[4/3] w-full bg-slate-950 object-cover" /> : <div className="flex aspect-[4/3] items-center justify-center bg-slate-950/60 px-3 text-center text-[10px] text-slate-500">页面图片文件不可用</div> : <div className="p-3"><div className="flex items-center gap-2 text-xs text-fuchsia-100"><FileText size={13} /><span className="truncate">{entry.title}</span></div><p className="mt-2 line-clamp-3 whitespace-pre-wrap text-[10px] leading-5 text-slate-400">{entry.text ?? "没有已保存的文档正文"}</p></div>}<div className="px-3 pb-2 pt-2">{entry.kind === "image" && <><div className="flex items-center gap-2 text-xs text-fuchsia-100"><ImageIcon size={13} /><span className="truncate">{entry.title}</span></div><p className="mt-1 line-clamp-2 whitespace-pre-wrap text-[10px] leading-5 text-slate-400">{entry.effectivePrompt ?? "没有已保存的页面提示词"}</p></>}<div className="mt-2 text-[9px] text-slate-600">{entry.pageNo ? `第 ${entry.pageNo} 页` : "作品级资料"}{entry.stale ? " · 已过期" : ""}</div></div></button>{importButtons([comicImportEntry(entry)], "mx-3 mb-3 mt-0")}</article>)}</div></div>)}</section>}
      {tasks.length > 0 && <section className="dream-panel rounded-2xl border border-white/6 p-4"><div className="mb-3 text-xs font-semibold uppercase tracking-[0.16em] text-slate-400">生成历史 · {tasks.length}</div><div className="max-h-64 space-y-1.5 overflow-y-auto pr-1">{tasks.map((task) => <div key={task.id} className="flex items-center justify-between rounded-lg border border-white/5 bg-white/[0.015] px-3 py-2 text-xs"><div className="min-w-0"><div className="truncate font-medium text-slate-200">{task.label}</div><div className="truncate text-[10px] text-slate-600">{new Date(task.createdAt).toLocaleString()}</div>{task.error && <div className="truncate text-[10px] text-rose-300/80">{task.error}</div>}</div>{task.status === "error" && task.params && <button title="按原参数重试" onClick={() => void retry(task)} disabled={Boolean(retrying)} className="rounded p-1 text-slate-500 hover:text-cyan-200 disabled:opacity-40"><RotateCcw size={12} className={retrying === task.id ? "animate-spin" : ""} /></button>}</div>)}</div></section>}
      {!groups.length && !showNovel && !showComic ? <div className="flex min-h-72 items-center justify-center rounded-2xl border border-dashed border-white/8 text-center"><div><div className="text-sm text-slate-400">{scope === "unassigned" ? "没有未归属资源" : "没有匹配的资源"}</div><div className="mt-2 text-xs text-slate-600">旧数据会保留在“其他历史”中。</div></div></div> : groups.map((bucket) => <section key={bucket.id}><div className="mb-3 flex flex-wrap items-center justify-between gap-2"><div className="text-xs font-semibold uppercase tracking-[0.16em] text-slate-400">{groupLabel(bucket)}</div>{bucket.group && (() => { const entries = bucket.assets.map(({ asset }) => libraryImportEntry(asset)); return bucket.group.type === "upload_batch" ? importButtons(entries, "mt-0", { image: "reference", video: "reference" }, (target) => uploadReferenceEntries(entries, target)) : importButtons(entries, "mt-0", "merge_prompt"); })()}</div><div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(170px,1fr))]">{bucket.assets.map(({ asset }) => { const entry = libraryImportEntry(asset); return asset.asset.kind === "text" ? <article key={asset.asset.id} className="rounded-xl border border-white/6 bg-slate-900/40 p-3 text-left"><button type="button" onClick={() => setPreview(asset)} className="w-full text-left hover:text-white"><div className="flex gap-2 text-xs text-slate-200"><FileText size={13} /><span className="truncate">{getDocumentMeta(asset)?.title ?? asset.source}</span></div><p className="mt-2 line-clamp-4 whitespace-pre-wrap text-[10px] leading-5 text-slate-500">{getDocumentMeta(asset)?.text || "点击读取完整文档"}</p></button>{importButtons([entry])}</article> : asset.asset.kind === "image" ? <article key={asset.asset.id} className="dream-panel overflow-hidden rounded-xl border border-white/6"><HistoryImageCard asset={asset} onOpen={() => setPreview(asset)} onMenu={(x, y) => setMenu({ x, y, asset })} /><div className="p-2.5"><div className="truncate text-[11px] text-slate-300">{asset.source}</div>{importButtons([entry])}</div></article> : <article key={asset.asset.id} className="dream-panel overflow-hidden rounded-xl border border-white/6 text-left"><button type="button" onClick={() => setPreview(asset)} onContextMenu={(event) => { event.preventDefault(); setMenu({ x: event.clientX, y: event.clientY, asset }); }} className="w-full text-left"><video src={convertFileSrc(asset.asset.path)} muted className="aspect-video w-full bg-black object-cover" /><div className="p-2.5"><div className="truncate text-[11px] text-slate-300">{asset.source}</div></div></button><div className="px-2.5 pb-2.5">{importButtons([entry], "mt-0")}</div></article>; })}</div></section>)}</div>
    {menu && <ContextMenu x={menu.x} y={menu.y} onClose={() => setMenu(null)} items={menu.asset.asset.kind === "image" ? imageHistoryMenu({ asset: menu.asset, onPreview: setPreview, onReference: (asset) => queueEntries([libraryImportEntry(asset)], "image", "reference") }) : [{ label: "查看完整详情", icon: menuIcons.preview, onClick: () => setPreview(menu.asset) }, { label: "复制文件路径", icon: menuIcons.copy, onClick: () => void navigator.clipboard?.writeText(menu.asset.asset.path).catch((error) => logEvent("warn", "clipboard.write_failed", { error: String(error) })) }, { label: "打开所在文件夹", icon: menuIcons.open, onClick: () => openFolder(menu.asset) }]} />}
    <AssetDetailModal asset={preview} onClose={() => setPreview(null)} onUseReference={(asset) => queueEntries([libraryImportEntry(asset)], "image", "reference")} />
    {comicPreview && <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-950/80 p-4" onClick={() => setComicPreview(null)}><section className="max-h-[85vh] w-full max-w-3xl overflow-auto rounded-2xl border border-fuchsia-300/20 bg-slate-950 p-5" onClick={(event) => event.stopPropagation()}><div className="flex items-start gap-3"><div className="min-w-0 flex-1"><h2 className="text-sm font-semibold text-fuchsia-100">{comicPreview.title}</h2><p className="mt-1 text-[10px] text-slate-500">漫画资料 · 只读</p></div><button type="button" onClick={() => setComicPreview(null)} className="text-slate-500 hover:text-white"><X size={16} /></button></div>{comicPreview.kind === "text" ? <pre className="mt-4 whitespace-pre-wrap break-words text-sm leading-7 text-slate-300">{comicPreview.text}</pre> : comicPreview.path ? <img src={convertFileSrc(comicPreview.path)} alt={comicPreview.title} className="mt-4 max-h-[65vh] w-full object-contain" /> : null}<div className="mt-4">{importButtons([comicImportEntry(comicPreview)], "mt-0")}</div><p className="mt-4 text-[10px] text-slate-500">此资料由漫画工作区维护，不能从这里删除。</p></section></div>}
    {activeId && <NovelAssetManager mode="manage" projectId={activeId} open={novelManagerOpen} onClose={() => setNovelManagerOpen(false)} initialSelection={focusedNovelWorkId ? { projectId: activeId, novelWorkId: focusedNovelWorkId } : undefined} onChanged={() => { void loadProjectSources(activeId); }} />}
  </div>;
}
