import { confirmAction } from "./lib/confirm";
import { lazy, Suspense, useEffect, useState } from "react";
import { BookOpen, Clapperboard, FolderCog, Image as ImageIcon, Images, Plus, RefreshCw, Settings, Trash2 } from "lucide-react";
import StatusBar from "./components/StatusBar";
import AppDocsDialog, { type AppDocsView } from "./components/AppDocsDialog";
import SettingsPage from "./components/SettingsPage";
import PromptManager from "./components/PromptManager";
import ProjectSettingsPage from "./components/ProjectSettingsPage";
import {
  appInfo,
  configStatus,
  listProviders,
  type AppInfo,
  type ConfigStatus,
  type ProviderInfo,
  acknowledgeHistoryRevision,
  markHistoryListenerReady,
} from "./lib/ipc";
import { getDb } from "./lib/db";
import { applyActive, loadSettings } from "./lib/settings";
import { loadHistory, refreshLibraryHistory } from "./lib/dbWrite";
import { useLibraryStore, type LibAsset } from "./store/useLibraryStore";
import { useProjectStore } from "./store/useProjectStore";
import { usePromptlibStore } from "./store/usePromptlibStore";
import { useGenerationStore } from "./store/useGenerationStore";
import { useVideoStore } from "./store/useVideoStore";
import { applyProjectProfile } from "./lib/projectProfile";
import { logEvent } from "./lib/logger";
import { listen } from "@tauri-apps/api/event";

type Mode = "image" | "video";
type Tab = "generate" | "comic" | "pipeline" | "assets";

const GeneratePanel = lazy(() => import("./pages/GeneratePanel"));
const VideoPanel = lazy(() => import("./pages/VideoPanel"));
const AgentPanel = lazy(() => import("./pages/AgentPanel"));
const AssetsPage = lazy(() => import("./pages/AssetsPage"));
const NovelComicPage = lazy(() => import("./pages/NovelComicPage"));

function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [dbReady, setDbReady] = useState(false);
  const [cfgStatus, setCfgStatus] = useState<ConfigStatus | null>(null);
  const [providers, setProviders] = useState<ProviderInfo[] | null>(null);
  const [mode, setMode] = useState<Mode>("image");
  const [tab, setTab] = useState<Tab>("generate");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [projectSettingsOpen, setProjectSettingsOpen] = useState(false);
  const [promptMgrOpen, setPromptMgrOpen] = useState(false);
  const [promptAgent, setPromptAgent] = useState("director");
  const [docsView, setDocsView] = useState<AppDocsView | null>(null);
  const [historyRefreshing, setHistoryRefreshing] = useState(false);
  const [historyRefreshNotice, setHistoryRefreshNotice] = useState<string | null>(null);
  const assetCount = useLibraryStore((s) => s.assets.length);
  const projects = useProjectStore((s) => s.projects);
  const activeProjectId = useProjectStore((s) => s.activeId);
  const activeProject = projects.find((p) => p.id === activeProjectId) ?? null;

  useEffect(() => {
    let cancelled = false;
    appInfo().then((a) => !cancelled && setInfo(a)).catch((error) => logEvent("error", "startup.app_info_failed", { error: String(error) }));
    listProviders().then((p) => !cancelled && setProviders(p)).catch((error) => logEvent("error", "startup.providers_failed", { error: String(error) }));
    configStatus().then((c) => !cancelled && setCfgStatus(c)).catch((error) => logEvent("error", "startup.config_status_failed", { error: String(error) }));

    getDb()
      .then(async () => {
        const s = await loadSettings();
        const st = await applyActive(s).catch(() => null);
        if (st && !cancelled) setCfgStatus(st);
        await useProjectStore.getState().load();
        await usePromptlibStore.getState().load();
        const projectState = useProjectStore.getState();
        const project = projectState.projects.find((p) => p.id === projectState.activeId);
        if (project) applyProjectProfile(project);
        const hist = await loadHistory().catch(() => ({ assets: [], tasks: [] }));
        if (!cancelled) {
          useLibraryStore.getState().loadAssets(hist.assets);
          useLibraryStore.getState().loadTasks(hist.tasks);
        }
        if (!cancelled) setDbReady(true);
      })
      .catch((error) => logEvent("error", "startup.database_failed", { error: String(error) }));

    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!dbReady) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<{ revision: number; operation: string; assetIds: string[]; changedAt: number }>("history://changed", (event) => {
      void refreshLibraryHistory("backend_event")
        .then(() => acknowledgeHistoryRevision(event.payload.revision))
        .catch(() => {});
    }).then((dispose) => {
      if (disposed) dispose();
      else {
        unlisten = dispose;
        void markHistoryListenerReady()
          .then(() => logEvent("info", "history.event_listener_ready", { event: "history://changed", polling: false }))
          .catch((error) => logEvent("error", "history.event_listener_ready_failed", { error: String(error) }));
      }
    }).catch((error) => logEvent("error", "history.event_listener_failed", { error: String(error) }));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [dbReady]);

  const refreshHistory = async () => {
    if (!dbReady || historyRefreshing) return;
    setHistoryRefreshing(true);
    setHistoryRefreshNotice(null);
    try {
      const history = await refreshLibraryHistory("global_header_button");
      setHistoryRefreshNotice(`历史已刷新：${history.assets.length} 个资源`);
      window.setTimeout(() => setHistoryRefreshNotice(null), 3000);
    } catch (error) {
      setHistoryRefreshNotice(`刷新失败：${String(error)}`);
    } finally {
      setHistoryRefreshing(false);
    }
  };

  const openAsset = (asset: LibAsset) => {
    if (asset.asset.kind === "video") {
      setMode("video");
      useVideoStore.getState().load((asset.params ?? {}) as Partial<import("./store/useVideoStore").VideoParams>);
    } else {
      setMode("image");
      useGenerationStore.getState().load({ ...(asset.params ?? {}), ...(asset.model ? { model: asset.model } : {}) } as Partial<import("./store/useGenerationStore").GenParams>);
    }
    setTab("generate");
  };

  const useReference = (asset: LibAsset) => {
    if (asset.asset.kind !== "image") return;
    setMode("image");
    useGenerationStore.getState().set({ referencePath: asset.asset.path });
    setTab("generate");
  };

  const switchProject = async (id: string) => {
    await useProjectStore.getState().switch(id);
    const project = useProjectStore.getState().projects.find((p) => p.id === id);
    if (project) applyProjectProfile(project);
    // 清空上一个项目遗留的私有生成参数，避免串项目（保留 musicPath）。
    useGenerationStore.getState().set({ prompt: "", referencePath: "" });
    useVideoStore.getState().set({ prompt: "", referencePath: "", musicEnabled: false });
  };

  const createProject = async () => {
    const id = await useProjectStore.getState().create();
    const project = useProjectStore.getState().projects.find((p) => p.id === id);
    if (project) applyProjectProfile(project);
    setProjectSettingsOpen(true);
  };

  const removeProject = async () => {
    if (!activeProjectId) return;
    const project = useProjectStore.getState().projects.find((item) => item.id === activeProjectId);
    if (!(await confirmAction(`确定删除项目“${project?.name ?? "当前项目"}”吗？项目列表会删除，但本地资产文件不会被自动删除。`))) return;
    await useProjectStore.getState().remove(activeProjectId);
    const state = useProjectStore.getState();
    const nextProject = state.projects.find((p) => p.id === state.activeId);
    if (nextProject) applyProjectProfile(nextProject);
  };

  return (
    <div className="app-shell flex h-screen flex-col text-slate-100">
      <header className="app-header flex flex-wrap items-center gap-3 border-b border-white/5 px-4 py-2 max-[1024px]:gap-x-2 max-[1024px]:gap-y-1.5">
        <div className="flex shrink-0 items-center gap-2 whitespace-nowrap">
          <img src="/app-icon.png" alt="" className="h-7 w-7 rounded-lg" />
          <span className="text-sm font-semibold">Image-Client</span>
        </div>

        {/* Project switcher (left) */}
        <div className="flex min-w-0 items-center gap-1.5 rounded-lg border border-slate-700 bg-slate-800/40 px-1 py-1 max-[1024px]:flex-1">
          <span className="pl-1 text-[11px] text-slate-500 whitespace-nowrap">项目</span>
          <select value={activeProjectId ?? ""} onChange={(e) => void switchProject(e.target.value).catch((error) => logEvent("error", "project.switch_failed", { error: String(error) }))} className="min-w-0 rounded-md border border-slate-700 bg-slate-900/70 px-2 py-1 text-xs text-slate-100 outline-none max-[1024px]:max-w-44 max-[1024px]:flex-1">
            {projects.map((p) => (<option key={p.id} value={p.id}>{p.name}</option>))}
          </select>
          <button title="新建项目" onClick={() => void createProject().catch((error) => logEvent("error", "project.create_failed", { error: String(error) }))} className="rounded-md p-1 text-slate-300 hover:bg-slate-700"><Plus size={14} /></button>
          <button title="项目生产档案" onClick={() => activeProject && setProjectSettingsOpen(true)} className="rounded-md p-1 text-slate-300 hover:bg-slate-700"><FolderCog size={13} /></button>
          <button title="删除项目" onClick={() => void removeProject().catch((error) => logEvent("error", "project.remove_failed", { error: String(error) }))} className="rounded-md p-1 text-slate-300 hover:bg-slate-700 hover:text-rose-300"><Trash2 size={13} /></button>
        </div>

        <nav className="flex gap-1 rounded-lg bg-slate-800/60 p-1 max-[1024px]:order-last max-[1024px]:basis-full max-[1024px]:flex-nowrap max-[1024px]:overflow-x-auto">
          <button onClick={() => setTab("generate")} className={`shrink-0 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition ${tab === "generate" ? "bg-slate-700 text-white" : "text-slate-400 hover:text-white"}`}>
            {mode === "video" ? "视频生成" : "图像生成"}
          </button>
          <button onClick={() => setTab("comic")} className={`flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition ${tab === "comic" ? "bg-slate-700 text-white" : "text-slate-400 hover:text-white"}`}>
            <BookOpen size={13} /> 小说漫画
          </button>
          <button onClick={() => setTab("pipeline")} className={`shrink-0 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition ${tab === "pipeline" ? "bg-slate-700 text-white" : "text-slate-400 hover:text-white"}`}>
            通用短剧流水线
          </button>
          <button onClick={() => setTab("assets")} className={`flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition ${tab === "assets" ? "bg-slate-700 text-white" : "text-slate-400 hover:text-white"}`}>
            <Images size={13} /> 资产库{assetCount > 0 ? <span className="rounded bg-slate-900/40 px-1 text-[9px]">{assetCount}</span> : null}
          </button>
        </nav>

        <div className="ml-auto flex shrink-0 items-center gap-3 max-[1024px]:ml-0 max-[1024px]:gap-1.5">
          {historyRefreshNotice && <span className="max-w-64 truncate text-[10px] text-cyan-100/70 max-[1024px]:hidden">{historyRefreshNotice}</span>}
          <button
            onClick={() => void refreshHistory()}
            disabled={!dbReady || historyRefreshing}
            title="立即从数据库刷新全部历史；外部接口成功时页面也会自动刷新"
            className="flex h-8 items-center gap-1.5 whitespace-nowrap rounded-lg border border-slate-700 px-2.5 text-[11px] text-slate-300 transition hover:border-cyan-300/20 hover:text-cyan-100 disabled:opacity-40 max-[1024px]:px-2"
          >
            <RefreshCw size={13} className={historyRefreshing ? "animate-spin" : ""} />
            {historyRefreshing ? "刷新中" : "刷新历史"}
          </button>
          {/* Mode toggle (right) */}
          <div className="flex shrink-0 gap-1 rounded-lg bg-slate-800/60 p-1">
            <button onClick={() => setMode("image")} className={`flex items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition ${mode === "image" ? "bg-indigo-500 text-white" : "text-slate-400 hover:text-white"}`}>
              <ImageIcon size={14} /> 图像
            </button>
            <button onClick={() => setMode("video")} className={`flex items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition ${mode === "video" ? "bg-fuchsia-500 text-white" : "text-slate-400 hover:text-white"}`}>
              <Clapperboard size={14} /> 视频
            </button>
          </div>
          <button onClick={() => setSettingsOpen(true)} title="设置" className="flex h-8 w-8 items-center justify-center rounded-lg border border-slate-700 text-slate-300 transition hover:bg-slate-800 hover:text-white">
            <Settings size={15} />
          </button>
        </div>
      </header>

      <div className="flex min-h-0 flex-1 flex-col">
        {tab === "pipeline" && <div className="border-b border-cyan-300/10 bg-slate-950/90 px-6 py-1.5 text-[10px] text-cyan-100/75">适用：通用短剧从剧本到逐镜出图；可串联或按需调用 Agent。</div>}
        <div className="flex min-h-0 flex-1">
          <Suspense fallback={<div className="flex flex-1 items-center justify-center text-sm text-slate-500">正在加载模块…</div>}>
            {tab === "generate" && (mode === "video" ? <VideoPanel /> : <GeneratePanel llmModel={cfgStatus?.llmModel} />)}
            {tab === "comic" && <NovelComicPage />}
            {tab === "pipeline" && <AgentPanel imageModel={cfgStatus?.imageModel} onEditPrompt={(id) => { setPromptAgent(id); setPromptMgrOpen(true); }} />}
            {tab === "assets" && <AssetsPage mode={mode} onUseReference={useReference} onOpenAsset={openAsset} />}
          </Suspense>
        </div>
      </div>

      <StatusBar
        appInfo={info}
        dbReady={dbReady}
        configStatus={cfgStatus}
        providers={providers}
        onOpenSettings={() => setSettingsOpen(true)}
        onOpenGuide={() => setDocsView("guide")}
        onOpenChangelog={() => setDocsView("changelog")}
      />

      <AppDocsDialog view={docsView} onClose={() => setDocsView(null)} />

      <SettingsPage open={settingsOpen} onClose={() => setSettingsOpen(false)} onSaved={setCfgStatus} status={cfgStatus} />

      <PromptManager open={promptMgrOpen} onClose={() => setPromptMgrOpen(false)} initialAgent={promptAgent} />

      <ProjectSettingsPage open={projectSettingsOpen} project={activeProject} onClose={() => setProjectSettingsOpen(false)} />
    </div>
  );
}

export default App;
