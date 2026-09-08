import { useEffect, useRef, useState } from "react";
import {
  newNovelIdempotencyKey,
  novelChapterRevisionCreate,
  novelWorkCreate,
  novelWorkGet,
  novelWorkList,
  type NovelChapter,
  type NovelChapterRevision,
  type NovelSnapshot,
  type NovelWork,
} from "../../lib/novel/api";

export interface NovelAssetSelection {
  projectId: string;
  novelWorkId: string;
  workTitle: string;
  novelChapterId: string;
  novelChapterRevisionId: string;
  /** The persisted original asset, if the chapter revision was imported from one. */
  sourceAssetId?: string;
  revisionNo: number;
  chapterNo: number;
  title: string;
  content: string;
}

export interface NovelAssetInitialSelection {
  projectId: string;
  novelWorkId: string;
  novelChapterId?: string;
}

type EditorMode = "browse" | "new_work" | "new_chapter" | "edit_chapter";
export type NovelAssetManagerMode = "manage" | "select";

export interface NovelAssetChange {
  projectId: string;
  novelWorkId: string;
  novelChapterId: string;
  novelChapterRevisionId: string;
}

export interface NovelAssetManagerProps {
  projectId: string;
  open: boolean;
  onClose: () => void;
  /** `select` returns an explicit revision only after the user chooses it; `manage` never does. */
  mode: NovelAssetManagerMode;
  onSelect?: (selection: NovelAssetSelection) => void;
  selectActionLabel?: string;
  /** Raised after the shared novel revision is durably saved, so a caller can refresh its own adaptation view. */
  onChanged?: (change: NovelAssetChange) => void;
  /** A caller's existing source to focus when browsing; never replaces an editor draft. */
  initialSelection?: NovelAssetInitialSelection;
}

const button = "rounded-lg border border-slate-700 px-3 py-2 text-xs text-slate-200 transition hover:border-cyan-300/30 hover:text-cyan-100 disabled:cursor-not-allowed disabled:opacity-40";
const primary = "rounded-lg bg-cyan-500 px-3 py-2 text-xs font-medium text-white transition hover:bg-cyan-400 disabled:cursor-not-allowed disabled:bg-slate-800 disabled:text-slate-500 disabled:opacity-70";
const field = "w-full rounded-xl border border-slate-700 bg-slate-950/60 px-3 py-2 text-sm text-slate-100 outline-none focus:border-cyan-300/50";

function activeWorks(items: NovelWork[]): NovelWork[] {
  return items.filter((item) => item.status !== "archived");
}

function revisionFor(chapter: NovelChapter | undefined, snapshot: NovelSnapshot | null): NovelChapterRevision | undefined {
  if (!chapter || !snapshot) return undefined;
  return snapshot.revisions?.find((item) => item.id === chapter.latestRevisionId);
}

type NovelAssetDraft = { content: string; title: string; chapterNo: number };

function draftStorageKeys(projectId: string, workId: string, chapterId?: string): string[] {
  const chapter = chapterId ?? "new";
  return [`novel-asset:source:${projectId}:${workId}:${chapter}`, `comic-md:source:${projectId}:${workId}:${chapter}`];
}

function readStoredDraft(projectId: string, workId: string, chapterId: string | undefined, fallback: NovelAssetDraft): { draft: NovelAssetDraft; restored: boolean } {
  for (const key of draftStorageKeys(projectId, workId, chapterId)) {
    try {
      const value: unknown = JSON.parse(localStorage.getItem(key) ?? "null");
      if (!value || typeof value !== "object" || Array.isArray(value)) continue;
      const source = value as Record<string, unknown>;
      if (typeof source.content !== "string" || typeof source.title !== "string" || typeof source.chapterNo !== "number") continue;
      return { draft: { content: source.content, title: source.title, chapterNo: Math.max(1, source.chapterNo) }, restored: true };
    } catch {
      // An invalid local draft must never be treated as saved source text.
    }
  }
  return { draft: fallback, restored: false };
}

function removeStoredDrafts(projectId: string, workId: string, chapterId?: string) {
  for (const key of draftStorageKeys(projectId, workId, chapterId)) {
    try { localStorage.removeItem(key); } catch { /* The durable revision remains the source of truth. */ }
  }
}

export default function NovelAssetManager({ projectId, open, onClose, mode: managerMode, onSelect, selectActionLabel = "引用此章节原文", onChanged, initialSelection }: NovelAssetManagerProps) {
  const [works, setWorks] = useState<NovelWork[]>([]);
  const [workId, setWorkId] = useState("");
  const [snapshot, setSnapshot] = useState<NovelSnapshot | null>(null);
  const [chapterId, setChapterId] = useState("");
  const [mode, setMode] = useState<EditorMode>("browse");
  const [newWorkTitle, setNewWorkTitle] = useState("");
  const [draft, setDraft] = useState({ chapterNo: 1, title: "", content: "" });
  const [loadingWorks, setLoadingWorks] = useState(false);
  const [loadingChapters, setLoadingChapters] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [snapshotStale, setSnapshotStale] = useState(false);
  const [lastSavedRevision, setLastSavedRevision] = useState<NovelChapterRevision | null>(null);
  const [editingScope, setEditingScope] = useState<{ projectId: string; workId: string; chapterId?: string } | null>(null);
  const [restoredDraft, setRestoredDraft] = useState(false);
  const [draftStorageError, setDraftStorageError] = useState(false);
  const worksRequestRef = useRef(0);
  const snapshotRequestRef = useRef(0);
  const sessionRef = useRef(0);
  const loadedProjectRef = useRef(projectId);
  const lastSavedRevisionRef = useRef<NovelChapterRevision | null>(null);
  const initialSelectionRef = useRef<NovelAssetInitialSelection | undefined>(initialSelection);
  const modeRef = useRef<EditorMode>(mode);
  const skipDraftPersistenceRef = useRef(new Set<string>());
  lastSavedRevisionRef.current = lastSavedRevision;
  initialSelectionRef.current = initialSelection;
  modeRef.current = mode;

  const reloadWorks = async (preferredWorkId?: string) => {
    const request = ++worksRequestRef.current;
    const session = sessionRef.current;
    setLoadingWorks(true);
    setError("");
    try {
      const items = activeWorks(await novelWorkList(projectId));
      if (request !== worksRequestRef.current || session !== sessionRef.current || loadedProjectRef.current !== projectId) return;
      setWorks(items);
      setWorkId((current) => modeRef.current === "browse" && preferredWorkId && items.some((item) => item.id === preferredWorkId)
        ? preferredWorkId
        : items.some((item) => item.id === current) ? current : items[0]?.id ?? "");
    } catch (cause) {
      if (request === worksRequestRef.current && session === sessionRef.current && loadedProjectRef.current === projectId) setError(`读取小说资产失败：${String(cause)}`);
    } finally {
      if (request === worksRequestRef.current && session === sessionRef.current && loadedProjectRef.current === projectId) setLoadingWorks(false);
    }
  };

  const reloadSnapshot = async (nextWorkId: string, preferredChapterId?: string): Promise<boolean> => {
    if (!nextWorkId) {
      setSnapshot(null);
      setChapterId("");
      setSnapshotStale(false);
      return true;
    }
    const request = ++snapshotRequestRef.current;
    const session = sessionRef.current;
    setLoadingChapters(true);
    setError("");
    try {
      const next = await novelWorkGet({ projectId, novelWorkId: nextWorkId });
      if (request !== snapshotRequestRef.current || session !== sessionRef.current || loadedProjectRef.current !== projectId) return false;
      setSnapshot(next);
      setSnapshotStale(false);
      const initial = initialSelectionRef.current;
      setChapterId((current) => {
        if (modeRef.current === "browse" && initial?.projectId === projectId && initial.novelWorkId === nextWorkId) {
          return initial.novelChapterId && next.chapters.some((item) => item.id === initial.novelChapterId) ? initial.novelChapterId : "";
        }
        return modeRef.current === "browse" && preferredChapterId && next.chapters.some((item) => item.id === preferredChapterId)
          ? preferredChapterId
          : next.chapters.some((item) => item.id === current) ? current : next.chapters[0]?.id ?? "";
      });
      const saved = lastSavedRevisionRef.current;
      if (saved?.novelWorkId === nextWorkId && next.revisions?.some((item) => item.id === saved.id)) {
        setChapterId(saved.chapterId);
        setMode("browse");
        setLastSavedRevision(null);
        setEditingScope(null);
      }
      return true;
    } catch (cause) {
      if (request === snapshotRequestRef.current && session === sessionRef.current && loadedProjectRef.current === projectId) {
        setSnapshot(null);
        setChapterId("");
        setSnapshotStale(true);
        setError(`读取章节失败：${String(cause)}`);
      }
      return false;
    } finally {
      if (request === snapshotRequestRef.current && session === sessionRef.current && loadedProjectRef.current === projectId) setLoadingChapters(false);
    }
  };

  useEffect(() => {
    loadedProjectRef.current = projectId;
    sessionRef.current += 1;
    worksRequestRef.current += 1;
    snapshotRequestRef.current += 1;
    setWorks([]);
    setWorkId("");
    setSnapshot(null);
    setChapterId("");
    setMode("browse");
    setDraft({ chapterNo: 1, title: "", content: "" });
    setLastSavedRevision(null);
    lastSavedRevisionRef.current = null;
    setEditingScope(null);
    setRestoredDraft(false);
    setDraftStorageError(false);
    setSnapshotStale(false);
    setError("");
    setNewWorkTitle("");
    setLoadingWorks(false);
    setLoadingChapters(false);
    setSaving(false);
  }, [projectId]);

  useEffect(() => {
    if (open) {
      sessionRef.current += 1;
      return;
    }
    sessionRef.current += 1;
    worksRequestRef.current += 1;
    snapshotRequestRef.current += 1;
    setSaving(false);
    setLoadingWorks(false);
    setLoadingChapters(false);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const initial = initialSelectionRef.current;
    void reloadWorks(mode === "browse" && initial?.projectId === projectId ? initial.novelWorkId : undefined);
    // The dialog intentionally retains unsaved form values across a close/reopen.
    // A request sequence prevents a late response from replacing a newer selection.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, projectId]);

  useEffect(() => {
    if (!open) return;
    const initial = initialSelectionRef.current;
    void reloadSnapshot(workId, mode === "browse" && initial?.projectId === projectId && initial.novelWorkId === workId ? initial.novelChapterId : undefined);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, workId]);

  useEffect(() => {
    const draftWorkId = editingScope?.workId;
    if (!open || (mode !== "new_chapter" && mode !== "edit_chapter") || !draftWorkId || editingScope?.projectId !== projectId) return;
    const chapter = editingScope?.chapterId;
    const [key] = draftStorageKeys(projectId, draftWorkId, chapter);
    if (skipDraftPersistenceRef.current.has(key)) return;
    try {
      localStorage.setItem(key, JSON.stringify(draft));
      setDraftStorageError(false);
    } catch {
      setDraftStorageError(true);
    }
  }, [draft, editingScope, mode, open, projectId]);

  if (!open) return null;

  const selectedWork = works.find((item) => item.id === workId);
  const currentSnapshot = snapshot?.work.id === workId ? snapshot : null;
  const selectedChapter = currentSnapshot?.chapters.find((item) => item.id === chapterId);
  const selectedRevision = revisionFor(selectedChapter, currentSnapshot);
  const nextChapterNo = Math.max(0, ...(currentSnapshot?.chapters.map((item) => item.chapterNo) ?? [])) + 1;
  const duplicateChapterNo = mode === "new_chapter" && Boolean(currentSnapshot?.chapters.some((item) => item.chapterNo === draft.chapterNo));

  const beginNewChapter = () => {
    const restored = readStoredDraft(projectId, workId, undefined, { chapterNo: nextChapterNo, title: "", content: "" });
    skipDraftPersistenceRef.current.delete(draftStorageKeys(projectId, workId)[0]);
    setDraft(restored.draft);
    setRestoredDraft(restored.restored);
    setLastSavedRevision(null);
    lastSavedRevisionRef.current = null;
    setEditingScope({ projectId, workId });
    setMode("new_chapter");
    setError("");
  };
  const beginEditChapter = () => {
    if (!selectedChapter || !selectedRevision) return;
    const restored = readStoredDraft(projectId, workId, selectedChapter.id, { chapterNo: selectedChapter.chapterNo, title: selectedChapter.title ?? "", content: selectedRevision.content });
    skipDraftPersistenceRef.current.delete(draftStorageKeys(projectId, workId, selectedChapter.id)[0]);
    setDraft(restored.draft);
    setRestoredDraft(restored.restored);
    setLastSavedRevision(null);
    lastSavedRevisionRef.current = null;
    setEditingScope({ projectId, workId, chapterId: selectedChapter.id });
    setMode("edit_chapter");
    setError("");
  };
  const createWork = async () => {
    if (saving || !newWorkTitle.trim()) return;
    const session = sessionRef.current;
    setSaving(true);
    setError("");
    try {
      const created = await novelWorkCreate({ projectId, title: newWorkTitle.trim(), idempotencyKey: newNovelIdempotencyKey("novel-asset-work") });
      if (session !== sessionRef.current || loadedProjectRef.current !== projectId) return;
      setNewWorkTitle("");
      setWorks((items) => items.some((item) => item.id === created.id) ? items : [...items, created]);
      setWorkId(created.id);
      setSnapshot({ work: created, chapters: [], revisions: [] });
      setChapterId("");
      setSnapshotStale(false);
      setMode("new_chapter");
      const restored = readStoredDraft(projectId, created.id, undefined, { chapterNo: 1, title: "", content: "" });
      skipDraftPersistenceRef.current.delete(draftStorageKeys(projectId, created.id)[0]);
      setDraft(restored.draft);
      setRestoredDraft(restored.restored);
      setLastSavedRevision(null);
      lastSavedRevisionRef.current = null;
      setEditingScope({ projectId, workId: created.id });
    } catch (cause) {
      if (session === sessionRef.current && loadedProjectRef.current === projectId) setError(`新建小说失败：${String(cause)}`);
    } finally {
      if (session === sessionRef.current && loadedProjectRef.current === projectId) setSaving(false);
    }
  };
  const saveChapter = async () => {
    const targetWorkId = editingScope?.workId ?? workId;
    const targetChapterId = editingScope?.chapterId;
    if (saving || lastSavedRevision || !targetWorkId || !draft.content.trim()) return;
    if (mode === "edit_chapter" && !targetChapterId) return;
    if (duplicateChapterNo) {
      setError(`第${draft.chapterNo}章已存在。请使用新的章节编号；现有章节不会被覆盖。`);
      return;
    }
    setSaving(true);
    setError("");
    const session = sessionRef.current;
    try {
      const revision = await novelChapterRevisionCreate({
        projectId,
        novelWorkId: targetWorkId,
        chapterId: mode === "edit_chapter" ? targetChapterId : undefined,
        chapterNo: draft.chapterNo,
        title: draft.title.trim() || `第${draft.chapterNo}章`,
        content: draft.content,
        idempotencyKey: newNovelIdempotencyKey(mode === "edit_chapter" ? "novel-asset-revision" : "novel-asset-chapter"),
      });
      if (session !== sessionRef.current || loadedProjectRef.current !== projectId) return;
      const [storedDraftKey] = draftStorageKeys(projectId, targetWorkId, targetChapterId);
      skipDraftPersistenceRef.current.add(storedDraftKey);
      removeStoredDrafts(projectId, targetWorkId, targetChapterId);
      setRestoredDraft(false);
      setLastSavedRevision(revision);
      lastSavedRevisionRef.current = revision;
      const refreshed = await reloadSnapshot(targetWorkId, revision.chapterId);
      if (session !== sessionRef.current || loadedProjectRef.current !== projectId) return;
      if (refreshed) {
        setChapterId(revision.chapterId);
        setMode("browse");
        setLastSavedRevision(null);
        lastSavedRevisionRef.current = null;
        setEditingScope(null);
      } else {
        setError("章节已保存，但重新读取失败。请先重试读取；在确认新修订前不会引用旧正文。你的编辑内容仍保留。");
      }
      try {
        onChanged?.({ projectId, novelWorkId: revision.novelWorkId, novelChapterId: revision.chapterId, novelChapterRevisionId: revision.id });
      } catch (cause) {
        setError(`章节已保存，但使用方刷新通知失败：${String(cause)}`);
      }
    } catch (cause) {
      if (session === sessionRef.current && loadedProjectRef.current === projectId) setError(`${mode === "edit_chapter" ? "保存章节修订" : "新增章节"}失败：${String(cause)}`);
    } finally {
      if (session === sessionRef.current && loadedProjectRef.current === projectId) setSaving(false);
    }
  };
  const selectChapter = () => {
    if (managerMode !== "select" || !onSelect || !selectedWork || !selectedChapter || !selectedRevision) return;
    onSelect({
      projectId,
      novelWorkId: selectedWork.id,
      workTitle: selectedWork.title,
      novelChapterId: selectedChapter.id,
      novelChapterRevisionId: selectedRevision.id,
      ...(selectedRevision.assetId ? { sourceAssetId: selectedRevision.assetId } : {}),
      revisionNo: selectedRevision.revisionNo,
      chapterNo: selectedChapter.chapterNo,
      title: selectedChapter.title ?? `第${selectedChapter.chapterNo}章`,
      content: selectedRevision.content,
    });
  };
  const retrySnapshot = async () => {
    const saved = lastSavedRevision;
    const refreshed = await reloadSnapshot(workId, saved?.chapterId);
    if (!refreshed) return;
    if (saved) {
      setChapterId(saved.chapterId);
      setMode("browse");
      setLastSavedRevision(null);
      lastSavedRevisionRef.current = null;
      setEditingScope(null);
    }
  };

  return <div className="fixed inset-0 z-[70] flex items-center justify-center bg-slate-950/80 p-4 backdrop-blur-sm" onMouseDown={(event) => { if (event.target === event.currentTarget && !saving) onClose(); }}>
    <section role="dialog" aria-modal="true" aria-labelledby="novel-source-picker-title" className="flex max-h-[90vh] w-full max-w-4xl flex-col overflow-hidden rounded-2xl border border-cyan-300/20 bg-slate-950 shadow-2xl">
      <header className="flex items-start justify-between gap-4 border-b border-slate-800 px-5 py-4">
          <div><h2 id="novel-source-picker-title" className="text-base font-semibold text-slate-100">{managerMode === "select" ? "选择小说原文资产" : "管理小说原文资产"}</h2><p className="mt-1 text-xs leading-5 text-slate-500">{managerMode === "select" ? "选择已保存的小说章节；原文与各工作台的改编内容分别保存。" : "在此维护共享小说、章节和原文版本；保存不会替换各工作台已保存的改编稿或原文快照。"}</p></div>
        <button className={button} disabled={saving} onClick={onClose}>关闭</button>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto p-5">
        {error && <p role="alert" className="mb-4 rounded-xl border border-rose-300/20 bg-rose-300/5 p-3 text-sm text-rose-100">{error}</p>}
        <div className="grid gap-5 lg:grid-cols-[minmax(220px,0.7fr)_minmax(0,1.3fr)]">
          <section className="space-y-3 rounded-xl border border-slate-800 bg-slate-950/40 p-4">
            <div className="flex items-center justify-between gap-2"><h3 className="text-sm font-semibold text-slate-200">小说资产</h3><button className={button} disabled={loadingWorks || saving || mode !== "browse"} onClick={() => void reloadWorks()}>{loadingWorks ? "读取中…" : "刷新"}</button></div>
            {works.length ? <label className="block text-xs text-slate-400">小说<select aria-label="小说资产" className={`${field} mt-1`} value={workId} disabled={saving || loadingChapters || mode !== "browse"} onChange={(event) => { setSnapshot(null); setChapterId(""); setSnapshotStale(false); setWorkId(event.target.value); setLastSavedRevision(null); lastSavedRevisionRef.current = null; }}>{works.map((item) => <option key={item.id} value={item.id}>{item.title}</option>)}</select>{mode !== "browse" && <span className="mt-1 block text-[11px] leading-4 text-slate-500">请先保存或取消当前章节编辑，再切换小说；草稿不会被静默丢弃。</span>}</label> : <p className="rounded-lg border border-dashed border-slate-700 p-3 text-xs leading-5 text-slate-500">当前项目还没有小说。先新建一本小说，再添加第一章；保存过程不会调用 AI。</p>}
            {mode === "new_work" ? <div className="space-y-2"><label className="block text-xs text-slate-400">小说名称<input aria-label="新小说名称" className={`${field} mt-1`} value={newWorkTitle} onChange={(event) => setNewWorkTitle(event.target.value)} autoFocus /></label><div className="flex gap-2"><button className={primary} disabled={saving || !newWorkTitle.trim()} onClick={() => void createWork()}>{saving ? "创建中…" : "创建并添加第一章"}</button><button className={button} disabled={saving} onClick={() => setMode("browse")}>取消</button></div></div> : <button className={button} disabled={saving || mode !== "browse"} onClick={() => setMode("new_work")}>新建小说</button>}
          </section>
          <section className="space-y-4 rounded-xl border border-slate-800 bg-slate-950/40 p-4">
            {loadingChapters ? <p className="py-10 text-sm text-slate-500">正在读取章节…</p> : !selectedWork ? <div><h3 className="text-sm font-semibold text-slate-200">从一本小说开始</h3><p className="mt-2 text-sm leading-6 text-slate-500">新建小说后即可在这里添加第一章。</p></div> : snapshotStale ? <div><h3 className="text-sm font-semibold text-amber-100">章节尚未重新读取</h3><p className="mt-2 text-sm leading-6 text-slate-500">不会使用缓存中的旧正文冒充刚保存的版本。请重新读取后再继续。</p><button className={`${primary} mt-4`} disabled={loadingChapters} onClick={() => void retrySnapshot()}>重新读取章节</button></div> : mode === "new_chapter" || mode === "edit_chapter" ? <div className="space-y-3"><div><h3 className="text-sm font-semibold text-slate-100">{mode === "new_chapter" ? `新增 ${selectedWork.title} 的章节` : `编辑第${editingScope?.chapterId === selectedChapter?.id ? selectedChapter?.chapterNo ?? draft.chapterNo : draft.chapterNo}章`}</h3><p className="mt-1 text-xs leading-5 text-slate-500">{mode === "new_chapter" ? "这是独立的新章节，不会覆盖现有章节。" : "保存会创建新的正文修订；各工作台已保存的原文快照不会自动替换。"}</p></div><div className="flex gap-3"><label className="w-28 text-xs text-slate-400">章节编号<input aria-label="章节编号" type="number" min={1} disabled={mode === "edit_chapter" || saving || Boolean(lastSavedRevision)} className={`${field} mt-1`} value={draft.chapterNo} onChange={(event) => setDraft((value) => ({ ...value, chapterNo: Math.max(1, Number(event.target.value) || 1) }))} /></label><label className="min-w-0 flex-1 text-xs text-slate-400">章节名称<input aria-label="章节名称" disabled={saving || Boolean(lastSavedRevision)} className={`${field} mt-1`} value={draft.title} onChange={(event) => setDraft((value) => ({ ...value, title: event.target.value }))} placeholder="可不填" /></label></div><label className="block text-xs text-slate-400">章节正文<textarea aria-label="小说章节正文" disabled={saving || Boolean(lastSavedRevision)} className={`${field} mt-1 min-h-64 resize-y leading-7`} value={draft.content} onChange={(event) => setDraft((value) => ({ ...value, content: event.target.value }))} placeholder="粘贴或编辑原文；保存不会调用 AI。" /></label>{restoredDraft && <p role="status" className="text-xs text-amber-200">已恢复未保存的小说原文草稿；它尚未保存或被引用。</p>}{draftStorageError && <p role="alert" className="text-xs text-amber-200">本地草稿暂时无法保存；关闭或刷新前请先复制正文。</p>}{duplicateChapterNo && <p role="alert" className="text-xs text-amber-200">第{draft.chapterNo}章已存在，请选择新的章节编号。</p>}<div className="flex flex-wrap gap-2"><button className={primary} disabled={saving || Boolean(lastSavedRevision) || duplicateChapterNo || !draft.content.trim()} onClick={() => void saveChapter()}>{saving ? "保存中…" : mode === "new_chapter" ? "保存新章节" : "保存新修订"}</button><button className={button} disabled={saving} onClick={() => { setLastSavedRevision(null); lastSavedRevisionRef.current = null; setEditingScope(null); setMode("browse"); }}>取消</button></div></div> : !currentSnapshot?.chapters.length ? <div><h3 className="text-sm font-semibold text-slate-100">这本小说还没有章节</h3><p className="mt-2 text-sm leading-6 text-slate-500">添加第一章后，才可继续管理或引用原文。</p><button className={`${primary} mt-4`} disabled={saving} onClick={beginNewChapter}>添加第一章</button></div> : <div className="space-y-4"><div className="flex flex-wrap items-start justify-between gap-3"><div><h3 className="text-sm font-semibold text-slate-100">选择章节</h3><p className="mt-1 text-xs text-slate-500">{managerMode === "select" ? "预览并确认后才会引用到当前工作台。" : "在此维护共享小说正文；各工作台已保存的改编稿与原文快照不会自动替换。"}</p></div><button className={button} disabled={saving} onClick={beginNewChapter}>添加新章节</button></div><label className="block text-xs text-slate-400">章节<select aria-label="小说章节" className={`${field} mt-1`} value={chapterId} disabled={saving} onChange={(event) => setChapterId(event.target.value)}><option value="" disabled>请选择章节</option>{[...currentSnapshot.chapters].sort((left, right) => (left.sequenceNo ?? left.chapterNo) - (right.sequenceNo ?? right.chapterNo)).map((chapter) => <option key={chapter.id} value={chapter.id}>第{chapter.chapterNo}章 · {chapter.title ?? "未命名"}</option>)}</select></label>{selectedChapter && selectedRevision ? <><div className="rounded-xl border border-cyan-300/15 bg-cyan-300/[0.04] p-3"><div className="flex flex-wrap items-center gap-2 text-xs text-cyan-100"><span>{selectedWork.title}</span><span>· 第{selectedChapter.chapterNo}章</span><span>· 正文第 {selectedRevision.revisionNo} 版</span></div><pre aria-label="章节正文预览" className="mt-3 max-h-60 overflow-auto whitespace-pre-wrap break-words font-sans text-sm leading-7 text-slate-300">{selectedRevision.content}</pre></div><div className="flex flex-wrap gap-2"><button className={button} disabled={saving} onClick={beginEditChapter}>编辑并保存新修订</button>{managerMode === "select" && <button className={primary} disabled={saving || !onSelect} onClick={selectChapter}>{selectActionLabel}</button>}</div></> : <p role="alert" className="text-sm text-amber-200">请选择一个具有当前正文修订的章节。</p>}</div>}
          </section>
        </div>
      </div>
    </section>
  </div>;
}
