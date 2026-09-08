import { loggedInvoke, logEvent } from "../logger";
import { saveDocumentVersion } from "../documents";
import { useLibraryStore } from "../../store/useLibraryStore";

/** Basic novel/source records used by the Markdown workspace. */
export interface NovelWork {
  id: string;
  projectId: string;
  title: string;
  description?: string | null;
  status: string;
  chapterCount?: number;
  createdAt?: number;
  updatedAt?: number;
}

export interface NovelChapter {
  id: string;
  novelWorkId: string;
  volumeId?: string | null;
  sequenceNo?: number | null;
  chapterNo: number;
  title: string | null;
  latestRevisionId?: string | null;
}

export interface NovelChapterRevision {
  id: string;
  novelWorkId: string;
  chapterId: string;
  revisionNo: number;
  content: string;
  contentHash?: string;
  /** Original project asset when this revision was imported from one; absent for pasted text. */
  assetId?: string | null;
  createdAt?: number;
}

export interface NovelSnapshot {
  work: NovelWork;
  chapters: NovelChapter[];
  revisions?: NovelChapterRevision[];
}

const input = <T>(command: string, value: unknown) => loggedInvoke<T>(command, { input: value });

export async function novelWorkList(projectId: string): Promise<NovelWork[]> {
  const result = await input<{ items: NovelWork[] } | NovelWork[]>("novel_work_list", { projectId, includeArchived: true });
  return Array.isArray(result) ? result : result.items;
}

export function novelWorkCreate(value: { projectId: string; title: string; description?: string; idempotencyKey: string }): Promise<NovelWork> {
  return input("novel_work_create", value);
}

/**
 * Chapter revisions already mirrored into the shared library this session, plus
 * the in-flight publish per revision. `novelWorkGet` is a read path, so it must
 * not re-issue one write per chapter on every call.
 */
const publishedRevisionIds = new Set<string>();
const publishingRevisionIds = new Map<string, Promise<void>>();

export function novelWorkGet(value: { projectId: string; novelWorkId: string }): Promise<NovelSnapshot> {
  return input<NovelSnapshot>("novel_work_get", value).then(async (snapshot) => {
    const revisions = snapshot.revisions ?? [];
    const wanted = new Map<string, { chapterNo: number; title: string; revision: NovelChapterRevision }>();
    for (const chapter of snapshot.chapters) {
      const revision = revisions.find((item) => item.id === chapter.latestRevisionId);
      if (!revision || publishedRevisionIds.has(revision.id)) continue;
      wanted.set(revision.id, { chapterNo: chapter.chapterNo, title: chapter.title ?? `第${chapter.chapterNo}章`, revision });
    }
    await Promise.all([...wanted.values()].map(({ chapterNo, title, revision }) => publishNovelChapter(
      value.projectId,
      snapshot.work.id,
      chapterNo,
      title,
      revision,
    )));
    return snapshot;
  });
}

async function publishNovelChapter(projectId: string, novelWorkId: string, chapterNo: number, title: string, revision: NovelChapterRevision) {
  const exists = useLibraryStore.getState().assets.some((asset) => asset.params?.novelChapterRevisionId === revision.id);
  if (exists) {
    publishedRevisionIds.add(revision.id);
    return;
  }
  const inFlight = publishingRevisionIds.get(revision.id);
  if (inFlight) return inFlight;
  const task = saveDocumentVersion({
    title,
    text: revision.content,
    projectId,
    documentType: "novel",
    changeType: "generated",
    agentId: "novel_source",
    metadata: { sourceKind: "novel_chapter", novelWorkId, novelChapterId: revision.chapterId, novelChapterRevisionId: revision.id, chapterNo },
    provenance: { originalInput: revision.content, generationInput: revision.content, sourceMaterials: [], parentAssetIds: [] },
  }).then(() => {
    publishedRevisionIds.add(revision.id);
  }).catch((error) => {
    logEvent("warn", "novel.shared_source_publish_failed", { revisionId: revision.id, error: String(error) });
  }).finally(() => {
    publishingRevisionIds.delete(revision.id);
  });
  publishingRevisionIds.set(revision.id, task);
  return task;
}

export function novelChapterRevisionCreate(value: {
  projectId: string;
  novelWorkId: string;
  chapterId?: string | null;
  chapterNo: number;
  title: string;
  content: string;
  idempotencyKey: string;
}): Promise<NovelChapterRevision> {
  return input<NovelChapterRevision>("novel_chapter_revision_create", value).then(async (revision) => {
    await publishNovelChapter(value.projectId, value.novelWorkId, value.chapterNo, value.title || `第${value.chapterNo}章`, revision);
    return revision;
  });
}

export function newNovelIdempotencyKey(prefix: string): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  return `${prefix}:${uuid ?? `${Date.now()}-${Math.random().toString(36).slice(2)}`}`;
}
