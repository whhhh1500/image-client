import { confirmAction } from "./lib/confirm";
import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { BookOpen, Clapperboard, FolderCog, Image as ImageIcon, Images, Plus, RefreshCw, Settings, Trash2, X } from "lucide-react";
import StatusBar, { openLogsFolder } from "./components/StatusBar";
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
import { useProjectStore, type Project } from "./store/useProjectStore";
import { usePromptlibStore } from "./store/usePromptlibStore";
import { useAgentStore } from "./store/useAgentStore";
import { useRunStore } from "./store/useRunStore";
import { useGenerationStore } from "./store/useGenerationStore";
import { useVideoStore } from "./store/useVideoStore";
import { bindFormsToProject, dropProjectDraft, formsProjectId, restoreProjectDrafts, stashProjectDrafts } from "./store/projectDrafts";
import { applyProjectProfile } from "./lib/projectProfile";
import { logEvent } from "./lib/logger";
import { errorText } from "./lib/errors";
import { listen } from "./lib/desktop";
import type { StoryboardShot } from "./lib/video/storyboard";
import { buildReviewedVideoHandoff } from "./lib/video/handoff";
import type { ImportEntry } from "./lib/assetImport";
import { queueGenerationAssetImport, useGenerationImportQueue } from "./store/useGenerationImportQueue";

type Mode = "image" | "video";
type Tab = "generate" | "comic" | "pipeline" | "assets";

/** Coalesce a burst of `history://changed` events into one library reload. */
const HISTORY_EVENT_DEBOUNCE_MS = 400;
/** How long a failed project create/switch/delete stays in the header. */
const PROJECT_NOTICE_MS = 6000;

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
  // A failed startup load used to be only logged: the status bar stayed on
  // "初始化中…" with nothing explaining why. It now raises a banner.
  const [startupError, setStartupError] = useState<string | null>(null);
  const [startupErrorHidden, setStartupErrorHidden] = useState(false);
  const startupRun = useRef(0);
  const [projectNotice, setProjectNotice] = useState<string | null>(null);
  const projectNoticeTimer = useRef<number | undefined>(undefined);
  const projects = useProjectStore((s) => s.projects);
  const activeProjectId = useProjectStore((s) => s.activeId);
  const activeProject = projects.find((p) => p.id === activeProjectId) ?? null;

  /** Loads backend info, settings, projects and history; 重试 runs it again. */
  const loadStartup = () => {
    // Only the latest run (and only while mounted) may write state.
    const run = ++startupRun.current;
    const current = () => run === startupRun.current;
    const fail = (event: string, prefix: string) => (error: unknown) => {
      logEvent("error", event, { error: String(error) });
      // Keep the first reason: a backend that is down fails every later step too.
      if (current()) setStartupError((shown) => shown ?? `${prefix}：${errorText(error)}`);
    };
    setStartupError(null);
    setStartupErrorHidden(false);
    appInfo().then((a) => current() && setInfo(a)).catch(fail("startup.app_info_failed", "无法连接后端"));
    listProviders().then((p) => current() && setProviders(p)).catch((error) => logEvent("error", "startup.providers_failed", { error: String(error) }));
    // The database step re-reads the config through applyActive, so a failure
    // here alone is not worth a banner.
    configStatus().then((c) => current() && setCfgStatus(c)).catch((error) => logEvent("error", "startup.config_status_failed", { error: String(error) }));

    getDb()
      .then(async () => {
        const s = await loadSettings();
        // These steps are independent; running them in parallel removes
        // serial IPC+SQL round-trips from cold start.
        const [st, , , , hist] = await Promise.all([
          applyActive(s).catch(() => null),
          usePromptlibStore.getState().load(),
          useProjectStore.getState().load(),
          useAgentStore.getState().load(),
          loadHistory().catch(() => ({ assets: [], tasks: [] })),
        ]);
        if (!current()) return;
        if (st) setCfgStatus(st);
        const projectState = useProjectStore.getState();
        const project = projectState.projects.find((p) => p.id === projectState.activeId);
        // A retry must not reset forms the user already moved to this project.
        if (project && formsProjectId() !== project.id) {
          applyProjectProfile(project);
          bindFormsToProject(project.id);
        }
        useLibraryStore.getState().loadAssets(hist.assets);
        useLibraryStore.getState().loadTasks(hist.tasks);
        setDbReady(true);
      })
      .catch(fail("startup.database_failed", "数据库或设置加载失败"));
  };

  useEffect(() => {
    loadStartup();
    return () => {
      startupRun.current += 1;
      window.clearTimeout(projectNoticeTimer.current);
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
      setHistoryRefreshNotice(`刷新失败：${errorText(error)}`);
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

  /** A project operation failure, shown briefly next to the project switcher. */
  const flashProjectNotice = (message: string) => {
    window.clearTimeout(projectNoticeTimer.current);
    setProjectNotice(message);
    projectNoticeTimer.current = window.setTimeout(() => setProjectNotice(null), PROJECT_NOTICE_MS);
  };
  const reportProjectFailure = (event: string, action: string) => (error: unknown) => {
    logEvent("error", event, { error: String(error) });
    flashProjectNotice(`${action}失败：${errorText(error)}`);
  };

  const sendStoryboardToVideo = async (shots: StoryboardShot[], source: LibAsset, approval: { anchorAssetId?: string; qcAssetId?: string; approvedModel: string; approvedAspectRatio: string; approvedResolution: string }) => {
    try {
      const handoff = buildReviewedVideoHandoff(shots, useLibraryStore.getState().assets, source, approval);
      if (!handoff.shots?.length) return;
      // The handoff replaces every shot. Ask before overwriting shots the user
      // has written; the empty default shot is not a draft.
      const drafted = useVideoStore.getState().shots.filter((shot) => shot.prompt.trim()).length;
      if (drafted) {
        const projectId = useProjectStore.getState().activeId;
        if (!(await confirmAction(`视频生成页已有 ${drafted} 个镜头草稿，将被当前分镜替换，继续？`))) return;
        // The form now belongs to another project; this storyboard does not.
        if (useProjectStore.getState().activeId !== projectId) return;
      }
      // The handoff replaces every shot; imports recorded for the previous shots
      // would otherwise be attributed to the reviewed shots with the same ids.
      useVideoStore.getState().set({ importedSources: [], ...handoff });
      setMode("video");
      setTab("generate");
    } catch (error) {
      logEvent("error", "storyboard.send_to_video_failed", { error: String(error) });
      flashProjectNotice(`发送分镜失败：${errorText(error)}`);
    }
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
    // The last run's batch preview and error belong to the previous project.
    useRunStore.getState().clearFinished();
  };

  /**
   * Hands the image/video forms to `project`. The outgoing project's unsaved
   * drafts are stashed, the incoming profile is applied and the project-scoped
   * fields cleared (references must not cross projects), then the incoming
   * project's own stashed drafts, if any, are put back over those defaults.
   */
  const moveFormsToProject = (project: Project | undefined, removedId?: string) => {
    stashProjectDrafts();
    if (removedId) dropProjectDraft(removedId);
    if (project) applyProjectProfile(project);
    clearProjectScopedGenerationState();
    restoreProjectDrafts(project?.id ?? null);
  };

  const switchProject = async (id: string) => {
    // A superseded switch (fast A→B) must not apply A's profile or wipe B's form.
    if (!(await useProjectStore.getState().switch(id))) return;
    // 先暂存上一个项目的草稿，再清空并恢复目标项目自己的草稿，避免跨项目引用资产。
    moveFormsToProject(useProjectStore.getState().projects.find((p) => p.id === id));
  };

  const createProject = async () => {
    const id = await useProjectStore.getState().create();
    moveFormsToProject(useProjectStore.getState().projects.find((p) => p.id === id));
    setProjectSettingsOpen(true);
  };

  const removeProject = async () => {
    if (!activeProjectId) return;
    const project = useProjectStore.getState().projects.find((item) => item.id === activeProjectId);
    if (!(await confirmAction(`确定删除项目“${project?.name ?? "当前项目"}”吗？项目列表会删除，但本地资产文件不会被自动删除。`))) return;
    await useProjectStore.getState().remove(activeProjectId);
    const state = useProjectStore.getState();
    moveFormsToProject(state.projects.find((p) => p.id === state.activeId), activeProjectId);
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
          <select value={activeProjectId ?? ""} onChange={(e) => void switchProject(e.target.value).catch(reportProjectFailure("project.switch_failed", "切换项目"))} className="min-w-0 rounded-md border border-slate-700 bg-slate-900/70 px-2 py-1 text-xs text-slate-100 outline-none max-[1024px]:max-w-44 max-[1024px]:flex-1">
            {projects.map((p) => (<option key={p.id} value={p.id}>{p.name}</option>))}
          </select>
          <button title="新建项目" onClick={() => void createProject().catch(reportProjectFailure("project.create_failed", "新建项目"))} className="rounded-md p-1 text-slate-300 hover:bg-slate-700"><Plus size={14} /></button>
          <button title="项目生产档案" onClick={() => activeProject && setProjectSettingsOpen(true)} className="rounded-md p-1 text-slate-300 hover:bg-slate-700"><FolderCog size={13} /></button>
          <button title="删除项目" onClick={() => void removeProject().catch(reportProjectFailure("project.remove_failed", "删除项目"))} className="rounded-md p-1 text-slate-300 hover:bg-slate-700 hover:text-rose-300"><Trash2 size={13} /></button>
        </div>
        {projectNotice && <span role="status" title={projectNotice} className="max-w-72 truncate text-[11px] text-rose-300">{projectNotice}</span>}

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
            {historyRefreshing ? "刷新中…" : "刷新历史"}
          </button>
          <button onClick={() => setSettingsOpen(true)} title="设置" className="flex h-8 w-8 items-center justify-center rounded-lg border border-slate-700 text-slate-300 transition hover:bg-slate-800 hover:text-white">
            <Settings size={15} />
          </button>
        </div>
      </header>

      {startupError && !startupErrorHidden && (
        <div role="alert" className="flex flex-wrap items-center gap-2 border-b border-rose-400/20 bg-rose-500/10 px-4 py-2 text-xs text-rose-100">
          <span className="min-w-0 flex-1 break-words">{startupError}</span>
          <button type="button" onClick={loadStartup} className="rounded-md border border-rose-300/30 px-2.5 py-1 text-rose-100 transition hover:bg-rose-500/20">
            重试
          </button>
          <button type="button" onClick={openLogsFolder} className="rounded-md border border-slate-600 px-2.5 py-1 text-slate-200 transition hover:bg-slate-800">
            打开日志
          </button>
          <button type="button" aria-label="关闭启动错误提示" onClick={() => setStartupErrorHidden(true)} className="rounded-md p-1 text-rose-200/70 transition hover:text-rose-100">
            <X size={13} />
          </button>
        </div>
      )}

      <div className="flex min-h-0 flex-1 flex-col">
        {tab === "pipeline" && <div className="border-b border-cyan-300/10 bg-slate-950/90 px-6 py-1.5 text-[10px] text-cyan-100/75">适用：从剧本、视觉锚点到 Markdown 视频分镜；确认后可逐镜发送到视频工作区。</div>}
        <div className="flex min-h-0 flex-1">
          <Suspense fallback={<div className="flex flex-1 items-center justify-center text-sm text-slate-500">正在加载模块…</div>}>
            {tab === "generate" && (mode === "video" ? <VideoPanel /> : <GeneratePanel llmModel={cfgStatus?.llmModel} defaultImageModel={cfgStatus?.imageReady ? cfgStatus.imageModel : undefined} />)}
            {tab === "comic" && <NovelComicPage />}
            {tab === "pipeline" && <VideoMarkdownWorkspace llmModel={cfgStatus?.llmModel} onEditPrompt={(id) => { setPromptAgent(id); setPromptMgrOpen(true); }} onSendToVideo={sendStoryboardToVideo} />}
            {tab === "assets" && <AssetsPage onQueueImport={queueAssetImport} />}
          </Suspense>
        </div>
      </div>

      <StatusBar
        appInfo={info}
        dbReady={dbReady}
        startupFailed={Boolean(startupError)}
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
