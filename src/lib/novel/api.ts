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

export function novelWorkGet(value: { projectId: string; novelWorkId: string }): Promise<NovelSnapshot> {
  return input<NovelSnapshot>("novel_work_get", value).then(async (snapshot) => {
    const revisions = snapshot.revisions ?? [];
    for (const chapter of snapshot.chapters) {
      const revision = revisions.find((item) => item.id === chapter.latestRevisionId);
      if (revision) await publishNovelChapter(value.projectId, snapshot.work.id, chapter.chapterNo, chapter.title ?? `第${chapter.chapterNo}章`, revision).catch((error) => logEvent("warn", "novel.shared_source_publish_failed", { revisionId: revision.id, error: String(error) }));
    }
    return snapshot;
  });
}

async function publishNovelChapter(projectId: string, novelWorkId: string, chapterNo: number, title: string, revision: NovelChapterRevision) {
  const exists = useLibraryStore.getState().assets.some((asset) => asset.params?.novelChapterRevisionId === revision.id);
  if (exists) return;
  await saveDocumentVersion({
    title,
    text: revision.content,
    projectId,
    documentType: "novel",
    changeType: "generated",
    agentId: "novel_source",
    metadata: { sourceKind: "novel_chapter", novelWorkId, novelChapterId: revision.chapterId, novelChapterRevisionId: revision.id, chapterNo },
    provenance: { originalInput: revision.content, generationInput: revision.content, sourceMaterials: [], parentAssetIds: [] },
  });
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
