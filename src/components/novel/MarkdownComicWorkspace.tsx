import { useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { useProjectStore } from "../../store/useProjectStore";
import { useComicMdWorkspace } from "../../store/useComicMdWorkspace";
import { novelWorkList, novelWorkCreate, novelWorkGet, novelChapterRevisionCreate, newNovelIdempotencyKey, type NovelWork, type NovelChapter } from "../../lib/novel/api";
import { comicMdExport, comicMdGenerate, comicMdRender, comicMdRenderOptionsSave, comicMdSync, mdLabels, mdStageBlock, type MdDocument, type MdScope, type MdStage, type MdWorkspace } from "../../lib/comic/markdownApi";
import MarkdownDocumentEditor from "./MarkdownDocumentEditor";
import MarkdownSyncNotice from "./MarkdownSyncNotice";
import { button, primary, field, scopeKey, useLocalValue, selectRenderPages, pageBlock, syncDraftBlock, type RenderSelection } from "./mdWorkspaceState";

export default function MarkdownComicWorkspace() {
  const projectId = useProjectStore((state) => state.activeId);
  return projectId ? <BookShelf key={projectId} projectId={projectId} /> : <p className="p-8 text-sm text-slate-400">请先选择或新建一个项目，再开始制作小说漫画。</p>;
}

function BookShelf({ projectId }: { projectId: string }) {
  const [works, setWorks] = useState<NovelWork[]>([]);
  const [selected, select] = useLocalValue(`comic-md:work:${projectId}`, "");
  const [title, setTitle] = useState("");
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState("");
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    void novelWorkList(projectId).then((items) => { if (alive.current) setWorks(items.filter((item) => item.status !== "archived")); }).catch((cause) => { if (alive.current) setError(String(cause)); });
    return () => { alive.current = false; };
  }, [projectId]);
  const create = async () => {
    if (creating || !title.trim()) return;
    setCreating(true); setError("");
    try {
      const work = await novelWorkCreate({ projectId, title: title.trim(), idempotencyKey: newNovelIdempotencyKey("md-work") });
      if (alive.current) { setWorks((items) => [...items, work]); select(work.id); setTitle(""); }
    } catch (cause) { if (alive.current) setError(String(cause)); } finally { if (alive.current) setCreating(false); }
  };
  const work = works.find((item) => item.id === selected) ?? works[0];
  return <section aria-label="Markdown 小说漫画" className="flex min-h-0 flex-1 flex-col overflow-y-auto">
    <header className="shrink-0 border-b border-slate-800 px-4 py-2">
      <p className="text-sm leading-5 text-slate-400">先写剧本和分镜，准备好后再画图。文字可独立复制使用。</p>
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <label className="text-sm text-slate-300">小说 <select aria-label="选择小说" className={`${field} ml-2 max-w-60`} value={work?.id ?? ""} onChange={(event) => select(event.target.value)}><option value="" disabled>请选择小说</option>{works.map((item) => <option key={item.id} value={item.id}>{item.title}</option>)}</select></label>
        <input aria-label="新小说名称" className={field} value={title} onChange={(event) => setTitle(event.target.value)} placeholder="输入新小说名称" />
        <button className={button} disabled={creating || !title.trim()} onClick={() => void create()}>{creating ? "创建中…" : "新建小说"}</button>
      </div>
      {error && <p role="alert" className="mt-2 text-sm text-rose-300">{error}</p>}
    </header>
    {work ? <ChapterShelf key={work.id} projectId={projectId} work={work} /> : <p className="p-6 text-sm text-slate-400">先新建一本小说，再粘贴第一章正文。保存文字不会调用模型。</p>}
  </section>;
}

function ChapterShelf({ projectId, work }: { projectId: string; work: NovelWork }) {
  const [chapters, setChapters] = useState<NovelChapter[]>([]);
  const [selected, select] = useLocalValue<string>(`comic-md:chapter:${projectId}:${work.id}`, "");
  const [error, setError] = useState("");
  const [loaded, setLoaded] = useState(false);
  const alive = useRef(true);
  const refresh = async () => {
    try { const snapshot = await novelWorkGet({ projectId, novelWorkId: work.id }); if (alive.current) { setChapters(snapshot.chapters); setLoaded(true); setError(""); } }
    catch (cause) { if (alive.current) setError(String(cause)); }
  };
  useEffect(() => { alive.current = true; void refresh(); return () => { alive.current = false; }; }, [projectId, work.id]);
  const chapter = selected === "new" ? undefined : chapters.find((item) => item.id === selected) ?? chapters[0];
  const nextNo = Math.max(0, ...chapters.map((item) => item.chapterNo)) + 1;
  const sorted = [...chapters].sort((a, b) => (a.sequenceNo ?? a.chapterNo) - (b.sequenceNo ?? b.chapterNo));
  return <div className="flex min-h-0 flex-1 flex-col">
    <div className="flex flex-wrap items-center gap-3 border-b border-slate-800 px-4 py-1">
      <label className="text-sm text-slate-300">章节 <select aria-label="选择章节" className={`${field} ml-2`} value={chapter?.id ?? "new"} onChange={(event) => select(event.target.value)}><option value="new">新章节</option>{sorted.map((item) => <option key={item.id} value={item.id}>第{item.chapterNo}章 · {item.title}</option>)}</select></label>
      <button className={button} onClick={() => select("new")}>添加下一章</button>
      {error && <p role="alert" className="text-sm text-rose-300">{error}<button className={button} onClick={() => void refresh()}>重新读取</button></p>}
    </div>
    {!loaded ? <p className="p-6 text-sm text-slate-400">正在读取章节…</p> : chapter ? <ChapterWorkspace key={chapter.id} scope={{ projectId, novelWorkId: work.id, chapterId: chapter.id }} chapter={chapter} refreshChapters={refresh} onChooseChapter={(id) => { if (chapters.some((item) => item.id === id)) select(id); }} /> : <div className="p-6"><SourceEditor key="new" projectId={projectId} workId={work.id} chapterNo={nextNo} onSaved={async (id) => { await refresh(); if (alive.current) select(id); }} /></div>}
  </div>;
}

function SourceEditor({ projectId, workId, chapter, chapterNo, sourceContent = "", onSaved }: { projectId: string; workId: string; chapter?: NovelChapter; chapterNo?: number; sourceContent?: string; onSaved: (id: string) => Promise<void> }) {
  const key = `comic-md:source:${projectId}:${workId}:${chapter?.id ?? "new"}`;
  const [draft, update, storageError] = useLocalValue<{ content: string; title: string; chapterNo: number } | null>(key, null);
  const value = draft ?? { content: sourceContent, title: chapter?.title ?? "", chapterNo: chapter?.chapterNo ?? chapterNo ?? 1 };
  const sourceChanged = !chapter || value.content !== sourceContent || value.title.trim() !== (chapter.title ?? "");
  const current = useRef(value); current.current = value;
  const active = useRef(true);
  useEffect(() => { active.current = true; return () => { active.current = false; }; }, []);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState("");
  const submit = async () => {
    if (saving || !value.content.trim() || !sourceChanged) return;
    const submitted = value;
    setSaving(true); setMessage("");
    try {
      const saved = await novelChapterRevisionCreate({ projectId, novelWorkId: workId, chapterId: chapter?.id, chapterNo: submitted.chapterNo, title: submitted.title || `第${submitted.chapterNo}章`, content: submitted.content, idempotencyKey: newNovelIdempotencyKey("md-source") });
      if (!active.current) return;
      if (chapter) {
        await onSaved(saved.chapterId);
        if (!active.current) return;
      }
      if (current.current === submitted) update(null);
      else if (!chapter) { try { localStorage.setItem(`comic-md:source:${projectId}:${workId}:${saved.chapterId}`, JSON.stringify(current.current)); } catch { /* The original draft remains available. */ } }
      if (!chapter) await onSaved(saved.chapterId);
      setMessage("正文已保存。接下来可手工填写或生成作品设定。");
    } catch (cause) { setMessage(String(cause)); } finally { setSaving(false); }
  };
  return <section aria-label="正文" className="space-y-4">
    <div><h2 className="text-lg font-semibold text-slate-100">{chapter ? "章节正文" : "放入新的章节"}</h2><p className="mt-1 text-sm text-slate-400">保存正文只存文字，不调用模型。修改正文后，已有剧本和提示词会保留并提示更新。</p></div>
    <div className="flex flex-wrap gap-3"><input aria-label="章节编号" type="number" min={1} disabled={!!chapter || saving} className={`${field} w-24`} value={value.chapterNo} onChange={(event) => update({ ...value, chapterNo: Math.max(1, Number(event.target.value) || 1) })} /><input aria-label="章节名称" className={`${field} flex-1`} value={value.title} onChange={(event) => update({ ...value, title: event.target.value })} placeholder="章节标题（可不填）" /></div>
    <textarea aria-label="章节正文" className={`${field} min-h-80 w-full resize-y leading-7`} value={value.content} onChange={(event) => update({ ...value, content: event.target.value })} placeholder="在这里粘贴小说正文" />
    {storageError && <p role="alert" className="text-sm text-amber-200">本地草稿暂时无法保存，请先复制文字再切换。</p>}
    <div className="flex flex-wrap items-center gap-3"><button className={primary} disabled={saving || !value.content.trim() || !sourceChanged} onClick={() => void submit()}>{saving ? "保存中…" : "保存正文"}</button>{sourceChanged && draft && <span className="text-sm text-amber-200">有未保存修改，切换后仍保留草稿</span>}{!sourceChanged && <span className="text-sm text-slate-500">正文和章节名称均已保存。</span>}</div>
    {message && <p role="status" className="text-sm text-slate-300">{message}</p>}
  </section>;
}

type View = "source" | "settings" | "script" | "storyboard" | "page_prompt" | "images";
const views: { id: View; label: string }[] = [{ id: "source", label: "正文" }, { id: "settings", label: "作品设定" }, { id: "script", label: "本章剧本" }, { id: "storyboard", label: "分页分镜" }, { id: "page_prompt", label: "每页 Prompt" }, { id: "images", label: "漫画" }];
function ChapterWorkspace({ scope, chapter, refreshChapters, onChooseChapter }: { scope: MdScope; chapter: NovelChapter; refreshChapters: () => Promise<void>; onChooseChapter: (chapterId: string) => void }) {
  const { workspace, error, refresh } = useComicMdWorkspace(scope);
  const [view, setView] = useState<View>("source");
  const [pageNo, setPageNo] = useState(1);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [exportPath, setExportPath] = useState("");
  const [injectionDraft, setInjectionDraft, injectionStorageError] = useLocalValue<{ promptInjection: string; expectedRevision: number } | null>(`comic-md:injection:${scopeKey(scope)}`, null);
  const injectionRef = useRef(injectionDraft); injectionRef.current = injectionDraft;
  const actionBusy = useRef(false);
  const active = useRef(true);
  useEffect(() => { active.current = true; return () => { active.current = false; }; }, []);
  const run = async (action: () => Promise<unknown>) => {
    if (actionBusy.current) return;
    actionBusy.current = true;
    setBusy(true); setMessage("");
    try { await action(); await refresh(); } catch (cause) { if (active.current) setMessage(String(cause)); } finally { actionBusy.current = false; if (active.current) setBusy(false); }
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
  const generate = () => run(() => comicMdGenerate({ ...scope, stage, expectedSourceRevisionId: workspace.sourceRevisionId! }));
  const renderPages = (documents: MdDocument[], rerunPromptInjection = "") => run(async () => {
    const reason = pageBlock(scope, documents);
    if (reason) throw new Error(reason);
    const options = await saveInjection();
    if (!active.current) return;
    return comicMdRender({
      ...scope,
      pages: documents.map((doc) => ({ documentId: doc.id, revision: doc.revision })),
      expectedRenderOptionsRevision: options.revision,
      ...(rerunPromptInjection.trim() ? { rerunPromptInjection } : {}),
    });
  });
  const renderChoices: { selection: RenderSelection; label: string }[] = [{ selection: "first", label: "生成第一页" }, { selection: "first_three", label: "生成前三页" }, { selection: "remaining", label: "生成剩余页" }];
  return <div className="flex min-h-0 flex-1 flex-col">
    <nav aria-label="制作步骤" className="flex shrink-0 flex-wrap gap-1 border-b border-slate-800 px-4 py-1">{views.map((item, index) => <button key={item.id} aria-current={view === item.id ? "step" : undefined} className={`${button} !py-1 ${view === item.id ? "border-indigo-400 bg-indigo-500/20 text-white" : "border-transparent"}`} onClick={() => setView(item.id)}>{index + 1}. {item.label}</button>)}</nav>
    <main className="min-h-0 flex-1 space-y-4 overflow-y-auto p-4">
      {(error || message) && <p role="alert" className="rounded-lg bg-amber-500/10 p-3 text-sm text-amber-100">{error || message}</p>}
      {currentJob && <p aria-label="当前生成状态" className="border-l-2 border-slate-600 pl-3 text-sm text-slate-300">{mdLabels[currentJob.kind]} · {currentJob.status === "running" ? `正在${currentJob.kind === "sync" ? "更新" : currentJob.kind === "optimize" ? "优化" : "生成"}${["images", "optimize", "sync"].includes(currentJob.kind) ? ` · ${currentJob.completedPages}/${currentJob.totalPages} ${currentJob.kind === "images" ? "页" : "份"}` : "，可以继续编辑文字"}` : "本次未完成，已有成果保留；请查看下方生成记录，修正后重新生成。"}</p>}
      <MarkdownSyncNotice scope={scope} workspace={workspace} disabled={disabled} onSync={sync} onChooseChapter={onChooseChapter} />
      {view === "source" ? <SourceEditor projectId={scope.projectId} workId={scope.novelWorkId} chapter={chapter} sourceContent={workspace.sourceContent} onSaved={async () => { await Promise.all([refresh(), refreshChapters()]); }} /> : view === "images" ? <>
        <div><h2 className="text-lg font-semibold text-slate-100">漫画结果</h2><p className="mt-1 text-sm leading-6 text-slate-400">一份页 Prompt 对应一张漫画页。按页码顺序生成；缺页、未保存或需更新时会明确提示。</p></div>
        <label className="block text-sm text-slate-300">本章 Prompt 注入<textarea aria-label="本章 Prompt 注入" className={`${field} mt-1 min-h-24 w-full`} value={injection} onChange={(event) => setInjectionDraft(event.target.value === renderOptions.promptInjection && (!injectionDraft || injectionDraft.expectedRevision === renderOptions.revision) ? null : { promptInjection: event.target.value, expectedRevision: injectionDraft?.expectedRevision ?? renderOptions.revision })} placeholder="例如：使用黑白水墨画风，所有对白用简体中文" /></label>
        <div className="flex flex-wrap items-center gap-2"><button className={button} disabled={busy || !injectionDirty} onClick={() => void run(async () => { await saveInjection(); setMessage("本章 Prompt 注入已保存。"); })}>保存注入</button><span className="text-sm text-slate-400">{injectionDirty ? "注入有未保存修改；点击任一生成按钮会先保存这份注入。" : `注入已保存 · 第${renderOptions.revision}版`}</span></div>
        <p className="text-sm text-slate-400">注入内容优先于每页 Prompt 中相冲突的要求；本章所有生图入口统一应用。修改后，旧规则生成的图片会标记需要更新。</p>
        {injectionDraft && injectionDraft.expectedRevision !== renderOptions.revision && <p role="alert" className="text-sm text-amber-200">注入规则已有较新版本，你的修改保留。<button className={`${button} ml-2`} onClick={() => setInjectionDraft(injection === renderOptions.promptInjection ? null : { promptInjection: injection, expectedRevision: renderOptions.revision })}>以当前注入版本为基准</button></p>}
        {injectionStorageError && <p role="alert" className="text-sm text-amber-200">注入草稿无法写入本地，请先复制内容。</p>}
        <div className="flex flex-wrap gap-3">{renderChoices.map(({ selection, label }) => { const choice = selectRenderPages(scope, workspace, selection, injection); return <div key={selection} className="max-w-72 space-y-1"><button className={primary} disabled={disabled || !!choice.reason} onClick={() => void renderPages(choice.documents)}>{label}{choice.documents.length ? `（${choice.documents.length} 页）` : ""}</button>{choice.reason && <p className="text-xs leading-5 text-amber-200">{choice.reason}</p>}</div>; })}</div>
        <p className="text-sm text-slate-400">点击生成会调用已配置的图片服务，可能计费。生成文字不会自动画图。</p>
        <ImageGallery key={scopeKey(scope)} scope={scope} workspace={workspace} onRender={renderPages} disabled={disabled} />
      </> : <>
        <div className="flex flex-wrap items-start justify-between gap-3"><div><h2 className="text-lg font-semibold text-slate-100">{mdLabels[view]}</h2><p className="mt-1 max-w-3xl text-sm leading-6 text-slate-400">{view === "settings" ? "整本小说共享的世界观、画风与人物基础特征。可以自己填写，也可以从本章正文生成。" : view === "script" ? "把故事写成可独立使用的剧本，补充本章带来的人物变化。" : view === "storyboard" ? "统一安排本章每页的剧情、每格画面和对白。用“# 第1页”等标题区分页。" : "每页一份完整提示词，包含本页需要的设定和人物信息。可以直接粘贴自己写好的 Prompt，保存检查后单独出图。"}</p></div><button className={primary} disabled={disabled || !!blocked} onClick={() => void generate()}>{running ? "任务进行中…" : `生成${mdLabels[stage]}（仅文字）`}</button></div>
        <p className="text-sm text-slate-400">{blocked || "生成文字会调用已配置的文本模型，可能计费。手工填写、保存和复制均不调用模型。"}</p>
        {view === "page_prompt" && <div className="flex flex-wrap items-center gap-2">{prompts.map((doc) => <button key={doc.id} className={`${button} ${pageNo === doc.pageNo ? "border-indigo-400" : ""}`} onClick={() => setPageNo(doc.pageNo!)}>第{doc.pageNo}页{doc.outOfPlan ? " · 不在当前分镜" : doc.stale ? " · 需更新" : doc.issues.length ? " · 待补齐" : ""}</button>)}<label className="text-sm text-slate-300">编辑第 <input aria-label="Prompt 页码" type="number" min={1} className={`${field} w-20`} value={pageNo} onChange={(event) => setPageNo(Math.max(1, Number(event.target.value) || 1))} /> 页</label><button className={button} onClick={() => setPageNo(Math.max(pageNo, ...prompts.map((doc) => doc.pageNo ?? 0)) + 1)}>添加下一页 Prompt</button></div>}
        <MarkdownDocumentEditor key={`${scopeKey(scope)}:${view}:${view === "page_prompt" ? pageNo : ""}`} scope={scope} kind={view} pageNo={view === "page_prompt" ? pageNo : undefined} document={currentDoc} pageDocuments={prompts} workspaceDocuments={workspace.documents} missingPageNos={workspace.syncPlan?.missingPageNos ?? []} jobs={jobs} refresh={refresh} renderingDisabled={disabled} onRender={renderPages} injection={injection} />
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
      <figcaption className="flex flex-wrap items-center justify-between gap-2 p-3 text-sm text-slate-200"><span>第 {pageNo} 页{chosen.stale ? " · 提示词或注入规则已更新" : ""}</span><select aria-label={`第${pageNo}页图片版本`} className={field} value={chosen.id} onChange={(event) => select({ ...selections, [pageNo]: event.target.value })}>{versions.map((image, index) => <option key={image.id} value={image.id}>{index === 0 ? "最新结果" : `历史结果 ${versions.length - index}`} · Prompt 第 {image.documentRevision} 版</option>)}</select></figcaption>
      <img src={convertFileSrc(chosen.path)} alt={`第${pageNo}页漫画`} className="mx-auto max-h-[70vh] max-w-full object-contain" onError={() => setError(`第 ${pageNo} 页图片无法显示，可打开文件位置检查。`)} />
      <div className="space-y-2 p-3">
        <label className="block text-sm text-slate-300">本次重画 Prompt 注入（可选）<textarea aria-label={`第${pageNo}页本次重画 Prompt 注入`} className={`${field} mt-1 min-h-20 w-full`} value={rerunInjection} onChange={(event) => updateRerunInjection(pageNo, event.target.value)} placeholder="例如：只把外套改为红色，其余画面保持不变" /></label>
        <p className="text-xs leading-5 text-slate-500">只作用于第 {pageNo} 页的这一次重画；与页 Prompt 或本章注入冲突时，以这里为准。</p>
        <div className="flex flex-wrap gap-2"><button className={button} onClick={() => setLargeImage({ path: chosen.path, pageNo })}>查看原图</button><button className={button} onClick={() => void revealItemInDir(chosen.path).catch((cause) => setError(String(cause)))}>打开文件位置</button><button className={primary} disabled={disabled || !!blocked} onClick={() => void onRender([prompt!], rerunInjection)}>重画第{pageNo}页</button></div>
      </div>
      {blocked && <p className="px-3 pb-2 text-xs text-amber-200">{blocked}</p>}
      <details className="px-3 pb-3 text-sm text-slate-400"><summary className="cursor-pointer">此图实际使用的注入规则</summary><div className="mt-2 space-y-2"><p className="whitespace-pre-wrap break-words"><strong className="text-slate-300">本章注入：</strong>{chosen.promptInjection || "未设置"}</p><p className="whitespace-pre-wrap break-words"><strong className="text-slate-300">本次重画注入：</strong>{chosen.rerunPromptInjection || "未设置"}</p></div></details>
    </figure>;
  })}</div>
  </>;
}
