import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { useProjectStore } from "../../store/useProjectStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useComicMdWorkspace } from "../../store/useComicMdWorkspace";
import { novelWorkGet, novelWorkList, type NovelChapter, type NovelWork } from "../../lib/novel/api";
import { comicMdExport, comicMdGenerate, comicMdRender, comicMdRenderOptionsSave, comicMdSync, comicMdWorkVisualExtract, comicMdWorkVisualSave, mdLabels, mdStageBlock, type MdDocument, type MdScope, type MdStage, type MdStyleReference, type MdWorkspace } from "../../lib/comic/markdownApi";
import { comicStyleReferenceKey, isComicStyleReferenceCandidate, normalizeComicStyleReferences } from "../../lib/comic/styleReferences";
import MarkdownDocumentEditor from "./MarkdownDocumentEditor";
import MarkdownSyncNotice from "./MarkdownSyncNotice";
import ComicStyleReferencePanel from "./ComicStyleReferencePanel";
import NovelAssetManager, { type NovelAssetChange, type NovelAssetInitialSelection, type NovelAssetManagerMode, type NovelAssetSelection } from "./NovelAssetManager";
import { button, primary, field, scopeKey, useLocalValue, selectRenderPages, pageBlock, syncDraftBlock, type RenderSelection } from "./mdWorkspaceState";

export default function MarkdownComicWorkspace() {
  const projectId = useProjectStore((state) => state.activeId);
  return projectId ? <BookShelf key={projectId} projectId={projectId} /> : <p className="p-8 text-sm text-slate-400">请先选择或新建一个项目，再开始制作小说漫画。</p>;
}

function BookShelf({ projectId }: { projectId: string }) {
  const [works, setWorks] = useState<NovelWork[]>([]);
  const [selected, select] = useLocalValue(`comic-md:work:${projectId}`, "");
  const [requestedChapterId, setRequestedChapterId] = useState<string>();
  const [managerOpen, setManagerOpen] = useState(false);
  const [managerMode, setManagerMode] = useState<NovelAssetManagerMode>("manage");
  const [managerInitial, setManagerInitial] = useState<NovelAssetInitialSelection>();
  const [sourceChange, setSourceChange] = useState<NovelAssetChange>();
  const [error, setError] = useState("");
  const alive = useRef(true);
  const refreshWorks = useCallback(async () => {
    try {
      const items = (await novelWorkList(projectId)).filter((item) => item.status !== "archived");
      if (alive.current) { setWorks(items); setError(""); }
    } catch (cause) { if (alive.current) setError(String(cause)); }
  }, [projectId]);
  useEffect(() => {
    alive.current = true;
    void refreshWorks();
    return () => { alive.current = false; };
  }, [refreshWorks]);
  const work = works.find((item) => item.id === selected) ?? works[0];
  const openManager = (mode: NovelAssetManagerMode, initial?: NovelAssetInitialSelection) => {
    setManagerMode(mode);
    setManagerInitial(initial ?? (work ? { projectId, novelWorkId: work.id } : undefined));
    setManagerOpen(true);
  };
  const chooseSource = (selection: NovelAssetSelection) => {
    if (selection.projectId !== projectId) return;
    setWorks((items) => items.some((item) => item.id === selection.novelWorkId)
      ? items
      : [...items, { id: selection.novelWorkId, projectId, title: selection.workTitle, status: "active" } as NovelWork]);
    select(selection.novelWorkId);
    setRequestedChapterId(selection.novelChapterId);
    setManagerOpen(false);
  };
  const changedSource = (change: NovelAssetChange) => {
    if (change.projectId !== projectId) return;
    setSourceChange(change);
    void refreshWorks();
  };
  return <section aria-label="Markdown 小说漫画" className="flex min-h-0 flex-1 flex-col overflow-y-auto">
    <header className="shrink-0 border-b border-slate-800 px-4 py-2">
      <p className="text-sm leading-5 text-slate-400">小说原文资产由统一管理器维护；本页保存的是独立的漫画改编稿、分镜和图片。管理原文不会自动覆盖这些成果。</p>
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <label className="text-sm text-slate-300">小说原文 <select aria-label="选择小说" className={`${field} ml-2 max-w-60`} value={work?.id ?? ""} onChange={(event) => { select(event.target.value); setRequestedChapterId(undefined); }}><option value="" disabled>请选择小说</option>{works.map((item) => <option key={item.id} value={item.id}>{item.title}</option>)}</select></label>
        <button className={button} onClick={() => openManager("manage")}>管理小说原文</button>
      </div>
      {error && <p role="alert" className="mt-2 text-sm text-rose-300">{error}</p>}
    </header>
    {work ? <ChapterShelf key={work.id} projectId={projectId} work={work} requestedChapterId={requestedChapterId} onRequestedChapterHandled={() => setRequestedChapterId(undefined)} sourceChange={sourceChange} onManage={openManager} /> : <div className="space-y-3 p-6"><p className="text-sm text-slate-400">先在共享小说原文管理器新建一本小说和第一章，再选择章节进入漫画改编。保存原文不会调用模型。</p><button className={button} onClick={() => openManager("manage")}>管理小说原文</button><button className={primary} onClick={() => openManager("select")}>选择小说章节开始漫画改编</button></div>}
    <NovelAssetManager projectId={projectId} open={managerOpen} onClose={() => setManagerOpen(false)} mode={managerMode} onSelect={chooseSource} onChanged={changedSource} initialSelection={managerInitial} selectActionLabel="在漫画中使用此章节" />
  </section>;
}

function ChapterShelf({ projectId, work, requestedChapterId, onRequestedChapterHandled, sourceChange, onManage }: { projectId: string; work: NovelWork; requestedChapterId?: string; onRequestedChapterHandled: () => void; sourceChange?: NovelAssetChange; onManage: (mode: NovelAssetManagerMode, initial?: NovelAssetInitialSelection) => void }) {
  const [chapters, setChapters] = useState<NovelChapter[]>([]);
  const [selected, select] = useLocalValue<string>(`comic-md:chapter:${projectId}:${work.id}`, "");
  const [error, setError] = useState("");
  const [loaded, setLoaded] = useState(false);
  const alive = useRef(true);
  const refresh = useCallback(async (): Promise<boolean> => {
    try {
      const snapshot = await novelWorkGet({ projectId, novelWorkId: work.id });
      if (alive.current) { setChapters(snapshot.chapters); setLoaded(true); setError(""); }
      return true;
    } catch (cause) {
      if (alive.current) setError(String(cause));
      return false;
    }
  }, [projectId, work.id]);
  useEffect(() => { alive.current = true; void refresh(); return () => { alive.current = false; }; }, [refresh]);
  useEffect(() => {
    if (sourceChange?.projectId === projectId && sourceChange.novelWorkId === work.id) void refresh();
  }, [projectId, refresh, sourceChange?.novelChapterRevisionId, sourceChange?.novelWorkId, sourceChange?.projectId, work.id]);
  useEffect(() => {
    if (requestedChapterId && chapters.some((item) => item.id === requestedChapterId)) {
      select(requestedChapterId);
      onRequestedChapterHandled();
    }
  }, [chapters, onRequestedChapterHandled, requestedChapterId, select]);
  const chapter = selected === "new" ? undefined : chapters.find((item) => item.id === selected) ?? chapters[0];
  const sorted = [...chapters].sort((a, b) => (a.sequenceNo ?? a.chapterNo) - (b.sequenceNo ?? b.chapterNo));
  return <div className="flex min-h-0 flex-1 flex-col">
    <div className="flex flex-wrap items-center gap-3 border-b border-slate-800 px-4 py-1">
      <label className="text-sm text-slate-300">漫画章节 <select aria-label="选择章节" className={`${field} ml-2`} value={chapter?.id ?? "new"} onChange={(event) => select(event.target.value)}><option value="new">选择或新增章节</option>{sorted.map((item) => <option key={item.id} value={item.id}>第{item.chapterNo}章 · {item.title}</option>)}</select></label>
      <button className={button} onClick={() => onManage("select", chapter ? { projectId, novelWorkId: work.id, novelChapterId: chapter.id } : undefined)}>选择 / 新增章节</button>
      {error && <p role="alert" className="text-sm text-rose-300">{error}<button className={button} onClick={() => void refresh()}>重新读取</button></p>}
    </div>
    {!loaded ? <p className="p-6 text-sm text-slate-400">正在读取章节…</p> : chapter ? <ChapterWorkspace key={chapter.id} scope={{ projectId, novelWorkId: work.id, chapterId: chapter.id }} chapter={chapter} refreshChapters={refresh} onChooseChapter={(id) => { if (chapters.some((item) => item.id === id)) select(id); }} sourceChange={sourceChange} onManageSource={() => onManage("manage", { projectId, novelWorkId: work.id, novelChapterId: chapter.id })} /> : <div className="space-y-3 p-6"><h2 className="text-lg font-semibold text-slate-100">选择小说章节开始漫画改编</h2><p className="text-sm leading-6 text-slate-400">小说原文在统一管理器中新增和编辑。选择已保存章节后，本页才会载入对应的漫画改编稿；已有章节的改编内容和图片不会被原文编辑自动覆盖。</p><button className={primary} onClick={() => onManage("select")}>管理并选择小说章节</button></div>}
  </div>;
}

type View = "source" | "settings" | "script" | "storyboard" | "page_prompt" | "images";
const views: { id: View; label: string }[] = [{ id: "source", label: "原文资产" }, { id: "settings", label: "作品设定" }, { id: "script", label: "本章剧本" }, { id: "storyboard", label: "分页分镜" }, { id: "page_prompt", label: "每页 Prompt" }, { id: "images", label: "漫画" }];
function ChapterWorkspace({ scope, chapter, refreshChapters, onChooseChapter, sourceChange, onManageSource }: { scope: MdScope; chapter: NovelChapter; refreshChapters: () => Promise<boolean>; onChooseChapter: (chapterId: string) => void; sourceChange?: NovelAssetChange; onManageSource: () => void }) {
  const { workspace, error, refresh } = useComicMdWorkspace(scope);
  const libraryAssets = useLibraryStore((state) => state.assets);
  const [view, setView] = useState<View>("source");
  const [visualOpen, setVisualOpen] = useState(false);
  useEffect(() => {
    setVisualOpen(view === "settings");
  }, [view]);
  const [pageNo, setPageNo] = useState(1);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [exportPath, setExportPath] = useState("");
  const [injectionDraft, setInjectionDraft, injectionStorageError] = useLocalValue<{ promptInjection: string; expectedRevision: number } | null>(`comic-md:injection:${scopeKey(scope)}`, null);
  const styleReferenceStorageKey = comicStyleReferenceKey(scope);
  const [styleReferenceDraft, setStyleReferenceDraft, styleReferenceStorageError] = useLocalValue<MdStyleReference[] | null>(styleReferenceStorageKey, null);
  const [constitutionDraft, setConstitutionDraft, constitutionStorageError] = useLocalValue<string | null>(`comic-md:visual-constitution:${scope.projectId}:${scope.novelWorkId}`, null);
  const [visualConflict, setVisualConflict] = useState(false);
  const injectionRef = useRef(injectionDraft); injectionRef.current = injectionDraft;
  const actionBusy = useRef(false);
  const active = useRef(true);
  useEffect(() => { active.current = true; return () => { active.current = false; }; }, []);
  useEffect(() => {
    if (sourceChange?.projectId === scope.projectId && sourceChange.novelWorkId === scope.novelWorkId && sourceChange.novelChapterId === scope.chapterId) {
      void Promise.all([refresh(), refreshChapters()]).then(([workspaceLoaded, chaptersLoaded]) => {
        if ((!workspaceLoaded || !chaptersLoaded) && active.current) setMessage("原文已保存，但漫画工作区重新读取失败；现有改编稿和草稿均未改动，请重试读取。");
      }).catch(() => { if (active.current) setMessage("原文已保存，但漫画工作区重新读取失败；现有改编稿和草稿均未改动，请重试读取。"); });
    }
  }, [refresh, refreshChapters, scope.chapterId, scope.novelWorkId, scope.projectId, sourceChange?.novelChapterId, sourceChange?.novelChapterRevisionId, sourceChange?.novelWorkId, sourceChange?.projectId]);
  const run = async (action: () => Promise<unknown>) => {
    if (actionBusy.current) return;
    actionBusy.current = true;
    setBusy(true); setMessage("");
    let actionError = "";
    try { await action(); } catch (cause) { actionError = String(cause); if (active.current) setMessage(actionError); }
    finally {
      try { await refresh(); } catch (cause) { if (active.current && !actionError) setMessage(String(cause)); }
      actionBusy.current = false;
      if (active.current) setBusy(false);
    }
  };
  if (!workspace) return <div className="p-6 text-sm text-slate-400">{error || "正在读取文字与漫画…"}{error && <button className={button} onClick={() => void refresh()}>重新读取</button>}</div>;
  const running = workspace.jobs.some((job) => job.status === "running");
  const jobs = workspace.jobs.slice().sort((a, b) => b.createdAt - a.createdAt);
  const currentJob = jobs.find((job) => job.status === "running") ?? (jobs[0]?.status === "failed" || jobs[0]?.status === "interrupted" ? jobs[0] : undefined);
  const disabled = busy || running;
  const prompts = workspace.documents.filter((doc) => doc.kind === "page_prompt").sort((a, b) => (a.pageNo ?? 0) - (b.pageNo ?? 0));
  const exportDocuments = workspace.documents.filter((doc) => !doc.outOfPlan);
  const sync = () => run(async () => {
    if (running) return;
    const blocked = syncDraftBlock(scope, workspace);
    if (blocked) throw new Error(blocked);
    await comicMdSync({ ...scope, expectedPlanFingerprint: workspace.syncPlan!.fingerprint });
    if (active.current) setMessage("已提交本章联动更新。只更新受影响文字，已有图片和历史版本保留。");
  });
  const renderOptions = workspace.renderOptions ?? { promptInjection: "", revision: 0 };
  const injection = injectionDraft?.promptInjection ?? renderOptions.promptInjection;
  const injectionDirty = !!injectionDraft && (injection !== renderOptions.promptInjection || injectionDraft.expectedRevision !== renderOptions.revision);
  const visualProfile = workspace.workVisualProfile ?? { constitutionMarkdown: "", revision: 0, references: [] };
  const constitution = constitutionDraft ?? visualProfile.constitutionMarkdown;
  const savedStyleReferences = visualProfile.references.map((reference, index) => {
    const path = reference.path || libraryAssets.find((asset) => asset.asset.id === reference.assetId)?.asset.path || "";
    return { assetId: reference.assetId, path, label: path.split(/[\\/]/).pop() || `画风参考 ${index + 1}`, description: reference.note || undefined };
  });
  const styleReferences = normalizeComicStyleReferences(styleReferenceDraft ?? savedStyleReferences);
  const referencesChanged = styleReferenceDraft !== null && (
    styleReferences.length !== visualProfile.references.length
    || styleReferences.some((reference, index) => reference.assetId !== visualProfile.references[index]?.assetId || (reference.description ?? "") !== (visualProfile.references[index]?.note ?? ""))
  );
  const invalidDraftReferences = referencesChanged ? styleReferences.filter((reference) => {
    const asset = libraryAssets.find((item) => item.asset.id === reference.assetId);
    return !asset || asset.asset.path !== reference.path || !isComicStyleReferenceCandidate(asset, scope);
  }) : [];
  const unavailableSavedReferences = referencesChanged ? [] : visualProfile.references.filter((reference) => !reference.fileAvailable);
  const visualReferencesForSave = referencesChanged
    ? styleReferences.map((reference, index) => ({ assetId: reference.assetId, role: "style", weight: 0.7, sortOrder: index, note: reference.description }))
    : visualProfile.references.map((reference) => ({ assetId: reference.assetId, role: reference.role, weight: reference.weight, sortOrder: reference.sortOrder, note: reference.note }));
  const visualDirty = constitution !== visualProfile.constitutionMarkdown || referencesChanged;
  const saveWorkVisual = async () => {
    if (!visualDirty) return visualProfile;
    if (visualConflict) throw new Error("作品视觉设定存在版本冲突，请先选择保留当前草稿或加载服务器版本");
    if (invalidDraftReferences.length) throw new Error(`有 ${invalidDraftReferences.length} 张画风参考尚未加载、已失效或不属于当前作品；请等待资源加载或主动移除后再保存`);
    try {
      const saved = await comicMdWorkVisualSave({
        projectId: scope.projectId,
        novelWorkId: scope.novelWorkId,
        constitutionMarkdown: constitution,
        references: visualReferencesForSave,
        expectedRevision: visualProfile.revision,
      });
      if (active.current) {
        setConstitutionDraft(null);
        setStyleReferenceDraft(null);
        setVisualConflict(false);
      }
      return saved;
    } catch (cause) {
      if (String(cause).includes("已有新版本")) {
        if (active.current) setVisualConflict(true);
        await refresh();
      }
      throw cause;
    }
  };
  const ensureVisualReady = (profile = visualProfile) => {
    const unavailable = profile.references.filter((reference) => !reference.fileAvailable);
    if (unavailable.length) throw new Error(`有 ${unavailable.length} 张已保存的画风参考文件不可用，请移除或重新上传后再生成`);
  };
  const extractWorkVisual = () => run(async () => {
    const saved = await saveWorkVisual();
    ensureVisualReady(saved);
    const instruction = styleReferences.flatMap((reference) => reference.description ? [`${reference.label}：${reference.description}`] : []).join("\n");
    const result = await comicMdWorkVisualExtract({ projectId: scope.projectId, novelWorkId: scope.novelWorkId, expectedRevision: saved.revision, ...(instruction ? { instruction } : {}) });
    if (active.current) {
      setConstitutionDraft(result.constitutionMarkdown);
      setMessage("已根据作品参考图生成可编辑的视觉宪法草稿；请检查后保存。");
    }
  });
  const saveInjection = async () => {
    if (!injectionDirty) return renderOptions;
    const submitted = injectionRef.current!;
    const options = await comicMdRenderOptionsSave({ ...scope, promptInjection: submitted.promptInjection, expectedRevision: submitted.expectedRevision });
    if (active.current) {
      if (injectionRef.current === submitted) setInjectionDraft(null);
      else if (injectionRef.current) setInjectionDraft({ ...injectionRef.current, expectedRevision: options.revision });
    }
    return options;
  };
  const currentDoc = workspace.documents.find((doc) => doc.kind === view && (view !== "page_prompt" || doc.pageNo === pageNo));
  const stage = view === "page_prompt" ? "page_prompts" : view as MdStage;
  const blocked = view !== "source" && view !== "images" ? mdStageBlock(stage, workspace) : null;
  const generate = () => run(async () => {
    const saved = await saveWorkVisual();
    ensureVisualReady(saved);
    return comicMdGenerate({ ...scope, stage, expectedSourceRevisionId: workspace.sourceRevisionId! });
  });
  const renderPages = (documents: MdDocument[], rerunPromptInjection = "") => run(async () => {
    const reason = pageBlock(scope, documents);
    if (reason) throw new Error(reason);
    const options = await saveInjection();
    const saved = await saveWorkVisual();
    ensureVisualReady(saved);
    if (!active.current) return;
    return comicMdRender({
      ...scope,
      pages: documents.map((doc) => ({ documentId: doc.id, revision: doc.revision })),
      expectedRenderOptionsRevision: options.revision,
      ...(rerunPromptInjection.trim() ? { rerunPromptInjection } : {}),
    });
  });
  const renderChoices: { selection: RenderSelection; label: string }[] = [{ selection: "first", label: "生成第一页" }, { selection: "first_three", label: "生成前三页" }, { selection: "remaining", label: "生成剩余页" }, ...((styleReferences.length || constitution.trim()) ? [{ selection: "all" as const, label: "按当前画风重画全部" }] : [])];
  return <div className="flex min-h-0 flex-1 flex-col">
    <nav aria-label="制作步骤" className="flex shrink-0 flex-wrap gap-1 border-b border-slate-800 px-4 py-1">{views.map((item, index) => <button key={item.id} aria-current={view === item.id ? "step" : undefined} className={`${button} !py-1 ${view === item.id ? "border-indigo-400 bg-indigo-500/20 text-white" : "border-transparent"}`} onClick={() => setView(item.id)}>{index + 1}. {item.label}</button>)}</nav>
    <main className="min-h-0 flex-1 space-y-4 overflow-y-auto p-4">
      {(error || message) && <p role="alert" className="rounded-lg bg-amber-500/10 p-3 text-sm text-amber-100">{error || message}</p>}
      {currentJob && <p aria-label="当前生成状态" className="border-l-2 border-slate-600 pl-3 text-sm text-slate-300">{mdLabels[currentJob.kind]} · {currentJob.status === "running" ? `正在${currentJob.kind === "sync" ? "更新" : currentJob.kind === "optimize" ? "优化" : "生成"}${["images", "optimize", "sync"].includes(currentJob.kind) ? ` · ${currentJob.completedPages}/${currentJob.totalPages} ${currentJob.kind === "images" ? "页" : "份"}` : "，可以继续编辑文字"}` : "本次未完成，已有成果保留；请查看下方生成记录，修正后重新生成。"}</p>}
      <MarkdownSyncNotice scope={scope} workspace={workspace} disabled={disabled} onSync={sync} onChooseChapter={onChooseChapter} />
      <details
        className="group rounded-xl border border-indigo-900/60 bg-indigo-950/20 p-4 transition"
        open={visualOpen}
        onToggle={(event) => setVisualOpen(event.currentTarget.open)}
      >
        <summary className="flex cursor-pointer select-none items-center justify-between font-medium text-indigo-100">
          <div className="flex items-center gap-2">
            <span className="text-sm font-semibold">作品视觉设定与画风参考（统一视觉宪法）</span>
            <span className="text-xs text-slate-400">
              {styleReferences.length ? `${styleReferences.length} 张参考图` : "无参考图"} · 第 {visualProfile.revision} 版{visualDirty ? " · 有未保存修改" : ""}
            </span>
          </div>
          <span className="text-xs text-indigo-300 underline-offset-2 group-open:text-slate-400">
            {visualOpen ? "收起" : "展开"}
          </span>
        </summary>
        <div className="mt-3 space-y-4">
          <ComicStyleReferencePanel scope={scope} references={styleReferences} onChange={setStyleReferenceDraft} disabled={disabled} />
          <section className="rounded-xl border border-indigo-300/15 bg-indigo-300/[0.035] p-4">
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div><h3 className="text-sm font-semibold text-indigo-100">统一视觉宪法</h3><p className="mt-1 max-w-3xl text-xs leading-5 text-slate-400">作品级画风会影响作品设定、剧本、分镜、每页 Prompt 和实际生图，只控制视觉语言，不改变小说剧情。</p></div>
              <div className="flex flex-wrap gap-2"><button className={button} disabled={disabled || !styleReferences.length || !!invalidDraftReferences.length || visualConflict} onClick={() => void extractWorkVisual()}>根据参考图提取</button><button className={primary} disabled={disabled || !visualDirty || !!invalidDraftReferences.length || visualConflict} onClick={() => void run(async () => { await saveWorkVisual(); setMessage("作品级画风参考和视觉宪法已保存。"); })}>保存作品视觉设定</button></div>
            </div>
            <textarea aria-label="作品级视觉宪法" className={`${field} mt-3 min-h-40 w-full`} value={constitution} onChange={(event) => setConstitutionDraft(event.target.value)} placeholder="先上传图片作品，再点击“根据参考图提取”；也可以直接编辑 Markdown。" />
            <p className="mt-2 text-xs text-slate-500">当前第 {visualProfile.revision} 版{visualDirty ? " · 有未保存修改" : " · 已保存"}。生成文字或图片前会先保存；修改后不会自动重画已有图片。</p>
            {!!invalidDraftReferences.length && <p role="alert" className="mt-2 text-xs text-amber-200">有 {invalidDraftReferences.length} 张本地草稿参考图尚未加载、已经失效或不属于当前作品。系统不会静默删除服务器中的参考图；请等待资源加载或主动移除。</p>}
            {!!unavailableSavedReferences.length && <p role="alert" className="mt-2 text-xs text-amber-200">有 {unavailableSavedReferences.length} 张已保存参考图的文件内容丢失或发生变化。生成已阻断，请移除并重新上传。</p>}
            {visualConflict && <div role="alert" className="mt-2 rounded-lg border border-amber-300/20 bg-amber-300/5 p-3 text-xs text-amber-100"><p>服务器中的作品视觉设定已有新版本，当前草稿未覆盖它。</p><div className="mt-2 flex flex-wrap gap-2"><button className={button} onClick={() => { setStyleReferenceDraft(null); setConstitutionDraft(null); setVisualConflict(false); setMessage("已加载服务器中的最新作品视觉设定。"); }}>加载服务器版本</button><button className={button} onClick={() => { setVisualConflict(false); setMessage("已保留当前草稿；再次保存时会以最新服务器版本为基准，请先核对内容。"); }}>保留当前草稿</button></div></div>}
            {(styleReferenceStorageError || constitutionStorageError) && <p role="alert" className="mt-2 text-xs text-amber-200">作品视觉草稿无法写入本地，请先复制内容。</p>}
          </section>
        </div>
      </details>
      {view === "source" ? <section aria-label="小说原文资产" className="space-y-4"><div className="flex flex-wrap items-start justify-between gap-3"><div><h2 className="text-lg font-semibold text-slate-100">小说原文资产</h2><p className="mt-1 max-w-3xl text-sm leading-6 text-slate-400">这是当前漫画章节使用的小说原文快照。编辑原文会新建原文修订，并只把现有漫画改编稿标记为需要核对；不会自动生成、覆盖剧本、分镜或已有图片。</p></div><button className={button} disabled={disabled} onClick={onManageSource}>管理小说原文</button></div><dl className="grid gap-3 text-sm sm:grid-cols-2"><div className="rounded-lg border border-slate-800 p-3"><dt className="text-xs text-slate-500">漫画章节</dt><dd className="mt-1 text-slate-200">第{chapter.chapterNo}章 · {chapter.title || "未命名章节"}</dd></div><div className="rounded-lg border border-slate-800 p-3"><dt className="text-xs text-slate-500">当前原文修订</dt><dd className="mt-1 break-all text-slate-200">{workspace.sourceRevisionId ?? "尚未保存原文"}</dd></div></dl><label className="block text-sm text-slate-300">小说原文预览<textarea aria-label="小说原文预览" readOnly className={`${field} mt-1 min-h-72 w-full resize-y leading-7`} value={workspace.sourceContent} /></label><p className="text-xs leading-5 text-slate-500">下面的“作品设定”“本章剧本”“分页分镜”和 Prompt 都是漫画改编稿；它们的保存和原文资产相互独立。</p></section> : view === "images" ? <>
        <div><h2 className="text-lg font-semibold text-slate-100">漫画结果</h2><p className="mt-1 text-sm leading-6 text-slate-400">一份页 Prompt 对应一张漫画页。按页码顺序生成；缺页、未保存或需更新时会明确提示。</p></div>
        <label className="block text-sm text-slate-300">本章 Prompt 注入<textarea aria-label="本章 Prompt 注入" className={`${field} mt-1 min-h-24 w-full`} value={injection} onChange={(event) => setInjectionDraft(event.target.value === renderOptions.promptInjection && (!injectionDraft || injectionDraft.expectedRevision === renderOptions.revision) ? null : { promptInjection: event.target.value, expectedRevision: injectionDraft?.expectedRevision ?? renderOptions.revision })} placeholder="例如：使用黑白水墨画风，所有对白用简体中文" /></label>
        <div className="flex flex-wrap items-center gap-2"><button className={button} disabled={busy || !injectionDirty} onClick={() => void run(async () => { await saveInjection(); setMessage("本章 Prompt 注入已保存。"); })}>保存注入</button><span className="text-sm text-slate-400">{injectionDirty ? "注入有未保存修改；点击任一生成按钮会先保存这份注入。" : `注入已保存 · 第${renderOptions.revision}版`}</span></div>
        <p className="text-sm text-slate-400">注入内容优先于每页 Prompt 中相冲突的要求；本章所有生图入口统一应用。修改后，旧规则生成的图片会标记需要更新。</p>
        {injectionDraft && injectionDraft.expectedRevision !== renderOptions.revision && <p role="alert" className="text-sm text-amber-200">注入规则已有较新版本，你的修改保留。<button className={`${button} ml-2`} onClick={() => setInjectionDraft(injection === renderOptions.promptInjection ? null : { promptInjection: injection, expectedRevision: renderOptions.revision })}>以当前注入版本为基准</button></p>}
        {injectionStorageError && <p role="alert" className="text-sm text-amber-200">注入草稿无法写入本地，请先复制内容。</p>}
        <div className="flex flex-wrap gap-3">{renderChoices.map(({ selection, label }) => { const choice = selectRenderPages(scope, workspace, selection, injection); return <div key={selection} className="max-w-72 space-y-1"><button className={primary} disabled={disabled || !!choice.reason} onClick={() => void renderPages(choice.documents)}>{label}{choice.documents.length ? `（${choice.documents.length} 页）` : ""}</button>{choice.reason && <p className="text-xs leading-5 text-amber-200">{choice.reason}</p>}</div>; })}</div>
        <p className="text-sm text-slate-400">点击生成会调用已配置的图片服务，可能计费。画风参考的变更只应用于新生成或主动重画的页面；旧图片会保留并标记需要更新。</p>
        <ImageGallery key={scopeKey(scope)} scope={scope} workspace={workspace} onRender={renderPages} disabled={disabled} />
      </> : <>
        <div className="flex flex-wrap items-start justify-between gap-3"><div><h2 className="text-lg font-semibold text-slate-100">{mdLabels[view]}</h2><p className="mt-1 max-w-3xl text-sm leading-6 text-slate-400">{view === "settings" ? "整本小说共享的世界观、画风与人物基础特征。可以自己填写，也可以从本章正文生成。" : view === "script" ? "把故事写成可独立使用的剧本，补充本章带来的人物变化。" : view === "storyboard" ? "统一安排本章每页的剧情、每格画面和对白。用“# 第1页”等标题区分页。" : "每页一份完整提示词，包含本页需要的设定和人物信息。可以直接粘贴自己写好的 Prompt，保存检查后单独出图。"}</p></div><button className={primary} disabled={disabled || !!blocked} onClick={() => void generate()}>{running ? "任务进行中…" : `生成${mdLabels[stage]}（仅文字）`}</button></div>
        <p className="text-sm text-slate-400">{blocked || "生成文字会调用已配置的文本模型，可能计费。手工填写、保存和复制均不调用模型。"}</p>
        {view === "page_prompt" && <div className="flex flex-wrap items-center gap-2">{prompts.map((doc) => <button key={doc.id} className={`${button} ${pageNo === doc.pageNo ? "border-indigo-400" : ""}`} onClick={() => setPageNo(doc.pageNo!)}>第{doc.pageNo}页{doc.outOfPlan ? " · 不在当前分镜" : doc.stale ? " · 需更新" : doc.issues.length ? " · 待补齐" : ""}</button>)}<label className="text-sm text-slate-300">编辑第 <input aria-label="Prompt 页码" type="number" min={1} className={`${field} w-20`} value={pageNo} onChange={(event) => setPageNo(Math.max(1, Number(event.target.value) || 1))} /> 页</label><button className={button} onClick={() => setPageNo(Math.max(pageNo, ...prompts.map((doc) => doc.pageNo ?? 0)) + 1)}>添加下一页 Prompt</button></div>}
        <MarkdownDocumentEditor key={`${scopeKey(scope)}:${view}:${view === "page_prompt" ? pageNo : ""}`} scope={scope} kind={view} pageNo={view === "page_prompt" ? pageNo : undefined} document={currentDoc} pageDocuments={prompts} workspaceDocuments={workspace.documents} missingPageNos={workspace.syncPlan?.missingPageNos ?? []} jobs={jobs} refresh={async () => { await refresh(); }} renderingDisabled={disabled} onRender={renderPages} beforeOptimize={async () => { const saved = await saveWorkVisual(); ensureVisualReady(saved); }} injection={injection} />
      </>}
      {!!exportDocuments.length && <div className="border-t border-slate-800 pt-4"><button className={button} disabled={busy} onClick={() => void run(async () => { const result = await comicMdExport({ ...scope, documentIds: exportDocuments.map((doc) => doc.id) }); setExportPath(result.files[0] ?? ""); setMessage(`已导出 ${result.files.length} 份 Markdown：${result.path}`); })}>导出本章全部已保存 MD</button><span className="ml-3 text-sm text-slate-500">包含作品设定，排除不在当前分镜的旧页；未保存草稿请先保存或复制。</span>{exportPath && <button className={`${button} ml-2`} onClick={() => void revealItemInDir(exportPath).catch((cause) => setMessage(String(cause)))}>打开导出文件位置</button>}</div>}
      {!!jobs.length && <details aria-label="生成记录" className="border-t border-slate-800 pt-3 text-sm text-slate-400"><summary className="cursor-pointer">生成记录（{jobs.length} 次）</summary>{jobs.map((job) => <div key={job.id} className="mt-3 border-l border-slate-700 pl-3"><p>{mdLabels[job.kind]} · {job.status === "running" ? "正在生成" : job.status === "succeeded" ? "已完成" : job.status === "interrupted" ? "已中断" : "未完成"}{job.kind === "images" && ` · ${job.completedPages}/${job.totalPages} 页`}</p>{job.message && <p className="mt-1">{job.message}</p>}{(job.status === "failed" || job.status === "interrupted") && <p className="mt-1">已有成果仍保留。修正后可点击对应步骤的生成按钮重新请求，可能计费。</p>}{job.outputMarkdown && <RawOutput markdown={job.outputMarkdown} />}</div>)}</details>}
    </main>
  </div>;
}

function RawOutput({ markdown }: { markdown: string }) {
  const [message, setMessage] = useState("");
  return <details className="mt-2"><summary className="cursor-pointer text-slate-400">查看本次生成的原始文字（可复制修正）</summary><textarea aria-label="生成原始 Markdown" readOnly value={markdown} className={`${field} mt-2 min-h-40 w-full`} /><button className={`${button} mt-2`} onClick={() => void navigator.clipboard.writeText(markdown).then(() => setMessage("已复制原始文字，可粘贴到下方编辑器修正。"), () => setMessage("复制失败，请在文字框中全选复制。"))}>复制原始文字</button><span role="status" className="ml-2">{message}</span></details>;
}

function visualReferenceCount(snapshot?: string): string {
  try {
    const parsed = JSON.parse(snapshot || "[]");
    return Array.isArray(parsed) ? `${parsed.length} 张` : "快照不可读";
  } catch {
    return "快照不可读";
  }
}

function ImageGallery({ scope, workspace, onRender, disabled }: { scope: MdScope; workspace: MdWorkspace; onRender: (documents: MdDocument[], rerunPromptInjection?: string) => Promise<void>; disabled: boolean }) {
  const [selections, select] = useLocalValue<Record<string, string>>(`comic-md:images:${scopeKey(scope)}`, {});
  const [rerunInjections, setRerunInjections, rerunStorageError] = useLocalValue<Record<string, string>>(`comic-md:rerun-injections:${scopeKey(scope)}`, {});
  const [error, setError] = useState("");
  const [largeImage, setLargeImage] = useState<{ path: string; pageNo: number } | null>(null);
  const pages = [...new Set(workspace.images.map((image) => image.pageNo))].sort((a, b) => a - b);
  if (!pages.length) return <p className="py-8 text-sm text-slate-400">还没有漫画图片。先准备每页 Prompt，再点击生成；也可以复制 Prompt 到其他工具画图。</p>;
  const updateRerunInjection = (pageNo: number, value: string) => {
    const next = { ...rerunInjections };
    if (value) next[pageNo] = value;
    else delete next[pageNo];
    setRerunInjections(next);
  };
  return <>
    {(error || rerunStorageError) && <p role="status" className="text-sm text-amber-200">{error || "本次重画注入草稿无法写入本地，请先复制内容。"}</p>}
    {largeImage && <div role="dialog" aria-modal="true" aria-label={`第${largeImage.pageNo}页原图`} className="fixed inset-0 z-50 flex flex-col bg-slate-950/95 p-6" onKeyDown={(event) => { if (event.key === "Escape") setLargeImage(null); }}><div className="flex justify-between gap-4 pb-4 text-slate-200"><span>第 {largeImage.pageNo} 页 · 原图</span><button autoFocus className={button} onClick={() => setLargeImage(null)}>关闭大图</button></div><div className="min-h-0 flex-1 overflow-auto"><img src={convertFileSrc(largeImage.path)} alt={`第${largeImage.pageNo}页漫画原图`} className="mx-auto h-auto" /></div></div>}
    <div className="grid gap-6 lg:grid-cols-2">{pages.map((pageNo) => {
    const versions = workspace.images.filter((image) => image.pageNo === pageNo).sort((a, b) => b.createdAt - a.createdAt);
    const chosen = versions.find((image) => image.id === selections[pageNo]) ?? versions[0];
    const prompt = workspace.documents.find((doc) => doc.id === chosen.documentId && doc.kind === "page_prompt");
    const blocked = prompt ? pageBlock(scope, [prompt]) : "本页 Prompt 不存在，请先恢复或保存。";
    const rerunInjection = rerunInjections[pageNo] ?? "";
    return <figure key={pageNo} className="overflow-hidden rounded-xl border border-slate-800">
      <figcaption className="flex flex-wrap items-center justify-between gap-2 p-3 text-sm text-slate-200"><span>第 {pageNo} 页{chosen.stale ? " · Prompt、注入或作品画风已更新" : ""}</span><select aria-label={`第${pageNo}页图片版本`} className={field} value={chosen.id} onChange={(event) => select({ ...selections, [pageNo]: event.target.value })}>{versions.map((image, index) => <option key={image.id} value={image.id}>{index === 0 ? "最新结果" : `历史结果 ${versions.length - index}`} · Prompt 第 {image.documentRevision} 版</option>)}</select></figcaption>
      <img src={convertFileSrc(chosen.path)} alt={`第${pageNo}页漫画`} className="mx-auto max-h-[70vh] max-w-full object-contain" onError={() => setError(`第 ${pageNo} 页图片无法显示，可打开文件位置检查。`)} />
      <div className="space-y-2 p-3">
        <label className="block text-sm text-slate-300">本次重画 Prompt 注入（可选）<textarea aria-label={`第${pageNo}页本次重画 Prompt 注入`} className={`${field} mt-1 min-h-20 w-full`} value={rerunInjection} onChange={(event) => updateRerunInjection(pageNo, event.target.value)} placeholder="例如：只把外套改为红色，其余画面保持不变" /></label>
        <p className="text-xs leading-5 text-slate-500">只作用于第 {pageNo} 页的这一次重画；与页 Prompt 或本章注入冲突时，以这里为准。</p>
        <div className="flex flex-wrap gap-2"><button className={button} onClick={() => setLargeImage({ path: chosen.path, pageNo })}>查看原图</button><button className={button} onClick={() => void revealItemInDir(chosen.path).catch((cause) => setError(String(cause)))}>打开文件位置</button><button className={primary} disabled={disabled || !!blocked} onClick={() => void onRender([prompt!], rerunInjection)}>重画第{pageNo}页</button></div>
      </div>
      {blocked && <p className="px-3 pb-2 text-xs text-amber-200">{blocked}</p>}
      <details className="px-3 pb-3 text-sm text-slate-400"><summary className="cursor-pointer">此图实际使用的视觉规则</summary><div className="mt-2 space-y-2"><p className="whitespace-pre-wrap break-words"><strong className="text-slate-300">作品视觉宪法：</strong>第 {chosen.visualProfileRevision ?? 0} 版</p><p className="whitespace-pre-wrap break-words"><strong className="text-slate-300">作品参考图：</strong>{visualReferenceCount(chosen.visualReferenceSnapshot)}</p><p className="whitespace-pre-wrap break-words"><strong className="text-slate-300">本章注入：</strong>{chosen.promptInjection || "未设置"}</p><p className="whitespace-pre-wrap break-words"><strong className="text-slate-300">本次重画注入：</strong>{chosen.rerunPromptInjection || "未设置"}</p></div></details>
    </figure>;
  })}</div>
  </>;
}
