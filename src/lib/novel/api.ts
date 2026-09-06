import { loggedInvoke } from "../logger";

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
  return input("novel_work_get", value);
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
  return input("novel_chapter_revision_create", value);
}

export function newNovelIdempotencyKey(prefix: string): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  return `${prefix}:${uuid ?? `${Date.now()}-${Math.random().toString(36).slice(2)}`}`;
}
