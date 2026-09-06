import { useEffect, useMemo, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { FileText, Image as ImageIcon, RotateCcw, Search, Video } from "lucide-react";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { ContextMenu, menuIcons } from "../components/ContextMenu";
import HistoryImageCard from "../components/HistoryImageCard";
import { imageHistoryMenu } from "../lib/historyMenu";
import { isCompressedAsset } from "../lib/imageCompress";
import AssetDetailModal from "../components/AssetDetailModal";
import { generateImage } from "../lib/generate";
import { generateVideo } from "../lib/generateVideo";
import type { GenParams } from "../store/useGenerationStore";
import type { VideoParams } from "../store/useVideoStore";
import { logEvent } from "../lib/logger";
import { getDocumentDisplayVersion, getDocumentMeta } from "../lib/documents";
import { refreshLibraryHistory } from "../lib/dbWrite";

const statusBadge: Record<string, string> = {
  running: "bg-amber-300/8 text-amber-200",
  success: "bg-cyan-300/8 text-cyan-200",
  error: "bg-rose-300/8 text-rose-200",
  idle: "bg-slate-500/10 text-slate-400",
};

type AssetView = "text" | "image" | "video";

export default function AssetsPage({
  mode,
  onUseReference,
  onOpenAsset,
}: {
  mode: "image" | "video";
  onUseReference?: (asset: LibAsset) => void;
  onOpenAsset?: (asset: LibAsset) => void;
}) {
  const allAssets = useLibraryStore((state) => state.assets);
  const allTasks = useLibraryStore((state) => state.tasks);
  const projects = useProjectStore((state) => state.projects);
  const activeId = useProjectStore((state) => state.activeId);
  const [view, setView] = useState<AssetView>(mode);
  const [query, setQuery] = useState("");
  const [menu, setMenu] = useState<{ x: number; y: number; asset: LibAsset } | null>(null);
  const [preview, setPreview] = useState<LibAsset | null>(null);
  const [retrying, setRetrying] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [refreshMessage, setRefreshMessage] = useState<string | null>(null);

  useEffect(() => setView(mode), [mode]);

  const defaultId = projects[0]?.id;
  const belongs = (projectId?: string) => projectId === activeId || (!projectId && activeId === defaultId);
  const assets = useMemo(() => {
    const term = query.trim().toLowerCase();
    return allAssets
      .filter((asset) => belongs(asset.projectId) && asset.asset.kind === view)
      .filter((asset) => {
        if (!term) return true;
        const document = getDocumentMeta(asset);
        return [asset.source, asset.model, document?.title, document?.text, asset.params?.prompt]
          .filter(Boolean)
          .some((value) => String(value).toLowerCase().includes(term));
      })
      .sort((a, b) => b.createdAt - a.createdAt);
  }, [activeId, allAssets, defaultId, query, view]);
  const tasks = allTasks
    .filter((task) => belongs(task.projectId) && (task.kind === view || (!task.kind && view === "image")))
    .sort((a, b) => b.createdAt - a.createdAt);

  const retryTask = async (task: (typeof tasks)[number]) => {
    if (!task.params || retrying) return;
    setRetrying(task.id);
    try {
      if (task.kind === "video") await generateVideo(task.params as unknown as VideoParams);
      else await generateImage(task.params as unknown as GenParams);
    } catch (error) {
      logEvent("warn", "task.retry_failed", { taskId: task.id, error: String(error) });
    } finally {
      setRetrying(null);
    }
  };

  const openFolder = (asset: LibAsset) => {
    void revealItemInDir(asset.asset.path).catch((error) => logEvent("warn", "asset.reveal_failed", { error: String(error) }));
  };

  const refreshHistory = async () => {
    if (refreshing) return;
    setRefreshing(true);
    setRefreshMessage(null);
    try {
      const history = await refreshLibraryHistory("asset_library_button");
      setRefreshMessage(`已刷新 · ${history.assets.length} 个资源`);
    } catch (error) {
      setRefreshMessage(`刷新失败：${String(error)}`);
    } finally {
      setRefreshing(false);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div className="flex flex-wrap items-center gap-2 border-b border-white/5 bg-slate-950/25 px-6 py-3">
        {(["text", "image", "video"] as AssetView[]).map((kind) => (
          <button key={kind} onClick={() => setView(kind)} className={`flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-xs transition ${view === kind ? "border-cyan-300/25 bg-cyan-300/10 text-cyan-100" : "border-white/5 text-slate-500 hover:border-white/15 hover:text-slate-300"}`}>
            {kind === "text" ? <FileText size={13} /> : kind === "image" ? <ImageIcon size={13} /> : <Video size={13} />}
            {kind === "text" ? "项目文档" : kind === "image" ? "图片" : "视频"}
            <span className="text-[9px] opacity-60">{allAssets.filter((asset) => belongs(asset.projectId) && asset.asset.kind === kind).length}</span>
          </button>
        ))}
        <label className="relative ml-auto min-w-56 flex-1 sm:max-w-sm">
          <Search size={13} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-600" />
          <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索标题、正文、模型或提示词" className="w-full rounded-lg border border-white/8 bg-slate-950/60 py-2 pl-8 pr-3 text-xs text-slate-200 outline-none focus:border-cyan-300/30" />
        </label>
        <button
          type="button"
          onClick={() => void refreshHistory()}
          disabled={refreshing}
          title="重新从数据库读取历史记录"
          className="flex items-center gap-1.5 rounded-lg border border-white/8 px-3 py-2 text-xs text-slate-400 transition hover:border-cyan-300/20 hover:text-cyan-100 disabled:opacity-50"
        >
          <RotateCcw size={13} className={refreshing ? "animate-spin" : ""} />
          {refreshing ? "刷新中" : "刷新历史"}
        </button>
        {refreshMessage && <span className="text-[10px] text-slate-500">{refreshMessage}</span>}
      </div>

      <div className="min-h-0 flex-1 space-y-6 overflow-y-auto p-6">
        {view !== "text" && tasks.length > 0 && (
          <section className="dream-panel rounded-2xl border border-white/6 p-4">
            <div className="mb-3">
              <div className="text-xs font-semibold uppercase tracking-[0.16em] text-slate-400">生成历史 · {tasks.length}</div>
              <div className="mt-1 text-[10px] text-slate-600">失败任务可按原参数重试；成功任务点击下方产物查看全部参数。</div>
            </div>
            <div className="max-h-64 space-y-1.5 overflow-y-auto pr-1">
              {tasks.map((task) => (
                <div key={task.id} className="flex items-center justify-between rounded-lg border border-white/5 bg-white/[0.015] px-3 py-2 text-xs">
                  <div className="min-w-0" title={task.error}>
                    <div className="truncate font-medium text-slate-200">{task.label}{task.model ? <span className="text-slate-600"> · {task.model}</span> : null}</div>
                    <div className="truncate text-[10px] text-slate-600">{new Date(task.createdAt).toLocaleString()}</div>
                    {task.error && <div className="truncate text-[10px] text-rose-300/80">{task.error}</div>}
                  </div>
                  <div className="ml-3 flex shrink-0 items-center gap-2">
                    {task.status === "error" && task.params && (
                      <button title="按原参数重试" onClick={() => void retryTask(task)} disabled={!!retrying} className="rounded p-1 text-slate-500 hover:bg-white/5 hover:text-cyan-200 disabled:opacity-40"><RotateCcw size={12} className={retrying === task.id ? "animate-spin" : ""} /></button>
                    )}
                    <span className={`rounded px-1.5 py-0.5 text-[9px] ${statusBadge[task.status]}`}>{task.status}</span>
                  </div>
                </div>
              ))}
            </div>
          </section>
        )}

        {!assets.length ? (
          <div className="flex min-h-72 items-center justify-center rounded-2xl border border-dashed border-white/8 text-center">
            <div>
              <div className="text-sm text-slate-400">{query ? "没有匹配的历史" : `当前项目还没有${view === "text" ? "文档" : view === "image" ? "图片" : "视频"}`}</div>
              <div className="mt-2 text-xs text-slate-600">{view === "text" ? "下一步：进入「剧本·分镜」运行任意 Agent，产物会自动保存并形成版本历史。" : `下一步：进入${view === "image" ? "图像" : "视频"}生成，或从文档历史导入提示词。`}</div>
            </div>
          </div>
        ) : view === "text" ? (
          <section>
            <div className="mb-3 text-xs font-semibold uppercase tracking-[0.16em] text-slate-400">全部文档版本 · {assets.length}</div>
            <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(270px,1fr))]">
              {assets.map((asset) => {
                const meta = getDocumentMeta(asset);
                return (
                  <button key={asset.asset.id} onClick={() => setPreview(asset)} onContextMenu={(event) => { event.preventDefault(); setMenu({ x: event.clientX, y: event.clientY, asset }); }} className="dream-panel group rounded-xl border border-white/6 p-4 text-left transition hover:-translate-y-0.5 hover:border-cyan-300/18">
                    <div className="flex items-center gap-2"><FileText size={14} className="text-cyan-200" /><span className="min-w-0 flex-1 truncate text-xs font-medium text-slate-200">{meta?.title ?? asset.source}</span><span className="text-[9px] text-cyan-200/60">v{getDocumentDisplayVersion(asset, assets)}</span></div>
                    <div className="mt-3 line-clamp-4 whitespace-pre-wrap text-[10px] leading-relaxed text-slate-600">{meta?.text || "点击读取完整文档"}</div>
                    <div className="mt-3 flex justify-between text-[9px] text-slate-700"><span>{meta?.documentType ?? "document"}</span><span>{new Date(asset.createdAt).toLocaleString()}</span></div>
                  </button>
                );
              })}
            </div>
          </section>
        ) : (
          <section>
            <div className="mb-3 text-xs font-semibold uppercase tracking-[0.16em] text-slate-400">{view === "image" ? "图片产物" : "视频产物"} · {assets.length}</div>
            <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(170px,1fr))]">
              {assets.filter((asset) => view !== "image" || !isCompressedAsset(asset)).map((asset) => (
                view === "image" ? (
                  <div key={asset.asset.id} className="dream-panel overflow-hidden rounded-xl border border-white/6">
                    <HistoryImageCard asset={asset} onOpen={() => setPreview(asset)} onMenu={(x, y) => setMenu({ x, y, asset })} />
                    <div className="p-2.5"><div className="truncate text-[11px] text-slate-300">{asset.source}</div><div className="mt-1 truncate text-[9px] text-slate-600">{asset.model || "无模型信息"} · {new Date(asset.createdAt).toLocaleString()}</div></div>
                  </div>
                ) : (
                <button key={asset.asset.id} title="点击查看完整详情和生成参数" onClick={() => setPreview(asset)} onContextMenu={(event) => { event.preventDefault(); setMenu({ x: event.clientX, y: event.clientY, asset }); }} className="dream-panel group overflow-hidden rounded-xl border border-white/6 text-left transition hover:-translate-y-0.5 hover:border-cyan-300/18">
                  <div className="aspect-video overflow-hidden bg-black/20">
                    <video src={convertFileSrc(asset.asset.path)} muted className="h-full w-full object-cover" />
                  </div>
                  <div className="p-2.5"><div className="truncate text-[11px] text-slate-300">{asset.source}</div><div className="mt-1 truncate text-[9px] text-slate-600">{asset.model || "无模型信息"} · {new Date(asset.createdAt).toLocaleString()}</div></div>
                </button>
                )
              ))}
            </div>
          </section>
        )}
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={menu.asset.asset.kind === "image"
            ? imageHistoryMenu({
                asset: menu.asset,
                onPreview: setPreview,
                onReference: onUseReference,
              })
            : [
                { label: "查看完整详情", icon: menuIcons.preview, onClick: () => setPreview(menu.asset) },
                { label: "复制文件路径", icon: menuIcons.copy, onClick: () => void navigator.clipboard?.writeText(menu.asset.asset.path).catch((error) => logEvent("warn", "clipboard.write_failed", { error: String(error) })) },
                { label: "打开所在文件夹", icon: menuIcons.open, onClick: () => openFolder(menu.asset) },
              ]}
        />
      )}

      <AssetDetailModal asset={preview} onClose={() => setPreview(null)} onLoadAsset={onOpenAsset} onUseReference={onUseReference} />
    </div>
  );
}
