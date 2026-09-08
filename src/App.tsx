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
import type { StoryboardShot } from "./lib/video/storyboard";
import { buildReviewedVideoHandoff } from "./lib/video/handoff";
import type { ImportEntry } from "./lib/assetImport";
import { queueGenerationAssetImport, useGenerationImportQueue } from "./store/useGenerationImportQueue";

type Mode = "image" | "video";
type Tab = "generate" | "comic" | "pipeline" | "assets";

/** Coalesce a burst of `history://changed` events into one library reload. */
const HISTORY_EVENT_DEBOUNCE_MS = 400;

const GeneratePanel = lazy(() => import("./pages/GeneratePanel"));
const VideoPanel = lazy(() => import("./pages/VideoPanel"));
const VideoMarkdownWorkspace = lazy(() => import("./components/video/VideoMarkdownWorkspace"));
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
        // These four steps are independent; running them in parallel removes
        // three serial IPC+SQL round-trips from cold start.
        const [st, , , hist] = await Promise.all([
          applyActive(s).catch(() => null),
          usePromptlibStore.getState().load(),
          useProjectStore.getState().load(),
          loadHistory().catch(() => ({ assets: [], tasks: [] })),
        ]);
        if (st && !cancelled) setCfgStatus(st);
        const projectState = useProjectStore.getState();
        const project = projectState.projects.find((p) => p.id === projectState.activeId);
        if (project) applyProjectProfile(project);
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
    let debounce: number | undefined;
    let pendingRevision = 0;
    void listen<{ revision: number; operation: string; assetIds: string[]; changedAt: number }>("history://changed", (event) => {
      // The REST API emits one event per produced asset, and each reload reads
      // the whole library. Coalesce a burst into a single refresh and always
      // acknowledge the newest revision so the backend can observe progress.
      pendingRevision = Math.max(pendingRevision, event.payload.revision);
      if (debounce !== undefined) window.clearTimeout(debounce);
      debounce = window.setTimeout(() => {
        debounce = undefined;
        const revision = pendingRevision;
        void refreshLibraryHistory("backend_event")
          .then(() => acknowledgeHistoryRevision(revision))
          .catch((error) => logEvent("error", "history.refresh_after_event_failed", { revision, error: String(error) }));
      }, HISTORY_EVENT_DEBOUNCE_MS);
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
      if (debounce !== undefined) window.clearTimeout(debounce);
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

  const queueAssetImport = (entries: ImportEntry[], target: "image" | "video", action: "prompt" | "merge_prompt" | "reference") => {
    if (!activeProjectId || !entries.length) return;
    queueGenerationAssetImport({ projectId: activeProjectId, entries, target, action });
    setMode(target);
    setTab("generate");
  };

  const sendStoryboardToVideo = (shots: StoryboardShot[], source: LibAsset, approval: { anchorAssetId?: string; qcAssetId?: string; approvedModel: string; approvedAspectRatio: string; approvedResolution: string }) => {
    const handoff = buildReviewedVideoHandoff(shots, useLibraryStore.getState().assets, source, approval);
    if (!handoff.shots?.length) return;
    useVideoStore.getState().set(handoff);
    setMode("video");
    setTab("generate");
  };

  const clearProjectScopedGenerationState = () => {
    useGenerationStore.getState().set({ prompt: "", referencePath: "", references: [], importedSources: [] });
    useVideoStore.getState().set({
      shots: [{ id: "shot-1", shotNo: 1, prompt: "", durationS: 5 }],
      images: [],
      videos: [],
      audios: [],
      storyboardSourceAssetId: undefined,
      productionManifest: undefined,
      importedSources: [],
      // With no retained references, reset newer stores to the backwards-compatible text mode.
      mode: "text",
    });
    useGenerationImportQueue.getState().discard();
  };

  const switchProject = async (id: string) => {
    // A superseded switch (fast A→B) must not apply A's profile or wipe B's form.
    if (!(await useProjectStore.getState().switch(id))) return;
    const project = useProjectStore.getState().projects.find((p) => p.id === id);
    if (project) applyProjectProfile(project);
    // 清空上一个项目的私有生成参数，避免跨项目引用资产。
    clearProjectScopedGenerationState();
  };

  const createProject = async () => {
    const id = await useProjectStore.getState().create();
    const project = useProjectStore.getState().projects.find((p) => p.id === id);
    if (project) applyProjectProfile(project);
    clearProjectScopedGenerationState();
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
    clearProjectScopedGenerationState();
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
          <button onClick={() => { setMode("image"); setTab("generate"); }} className={`flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition ${tab === "generate" && mode === "image" ? "bg-indigo-500 text-white" : "text-slate-400 hover:text-white"}`}>
            <ImageIcon size={13} /> 图像生成
          </button>
          <button onClick={() => { setMode("video"); setTab("generate"); }} className={`flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition ${tab === "generate" && mode === "video" ? "bg-fuchsia-500 text-white" : "text-slate-400 hover:text-white"}`}>
            <Clapperboard size={13} /> 视频生成
          </button>
          <button onClick={() => setTab("comic")} className={`flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition ${tab === "comic" ? "bg-slate-700 text-white" : "text-slate-400 hover:text-white"}`}>
            <BookOpen size={13} /> 小说漫画
          </button>
          <button onClick={() => setTab("pipeline")} className={`shrink-0 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition ${tab === "pipeline" ? "bg-slate-700 text-white" : "text-slate-400 hover:text-white"}`}>
            短剧 Agent
          </button>
          <button onClick={() => setTab("assets")} className={`flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition ${tab === "assets" ? "bg-slate-700 text-white" : "text-slate-400 hover:text-white"}`}>
            <Images size={13} /> 资产库
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
          <button onClick={() => setSettingsOpen(true)} title="设置" className="flex h-8 w-8 items-center justify-center rounded-lg border border-slate-700 text-slate-300 transition hover:bg-slate-800 hover:text-white">
            <Settings size={15} />
          </button>
        </div>
      </header>

      <div className="flex min-h-0 flex-1 flex-col">
        {tab === "pipeline" && <div className="border-b border-cyan-300/10 bg-slate-950/90 px-6 py-1.5 text-[10px] text-cyan-100/75">适用：从剧本、视觉锚点到 Markdown 视频分镜；确认后可逐镜发送到视频工作区。</div>}
        <div className="flex min-h-0 flex-1">
          <Suspense fallback={<div className="flex flex-1 items-center justify-center text-sm text-slate-500">正在加载模块…</div>}>
            {tab === "generate" && (mode === "video" ? <VideoPanel /> : <GeneratePanel llmModel={cfgStatus?.llmModel} />)}
            {tab === "comic" && <NovelComicPage />}
            {tab === "pipeline" && <VideoMarkdownWorkspace llmModel={cfgStatus?.llmModel} onEditPrompt={(id) => { setPromptAgent(id); setPromptMgrOpen(true); }} onSendToVideo={sendStoryboardToVideo} />}
            {tab === "assets" && <AssetsPage onQueueImport={queueAssetImport} />}
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
