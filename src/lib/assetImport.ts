import type { LibAsset } from "../store/useLibraryStore";
import { getDocumentMeta } from "./documents";
import { snapshotAsset, type SourceMaterialSnapshot } from "./provenance";

/**
 * A serializable record of an explicit library import. It is kept with the
 * form params, then written unchanged into the generated asset's params.
 * `sourceMaterials` is deliberately a snapshot, not a later lookup of a
 * mutable library record.
 */
export interface AssetImportRecord {
  action: "prompt" | "merge_prompt" | "reference";
  /** Video imports are attached to one editable shot, so replacing shot 2
   * never erases the audit record for shot 1. */
  targetShotId?: string;
  assetIds: string[];
  sourceMaterials: SourceMaterialSnapshot[];
}

/** A read-only catalog item without inventing an assets-table ID. */
export interface CanonicalImportEntry {
  entryType: "canonical_comic";
  readonly: true;
  sourceUri: string;
  projectId: string;
  kind: "text" | "image";
  title: string;
  text?: string;
  /** Exact immutable prompt used by a canonical comic page render. */
  effectivePrompt?: string;
  path?: string;
  /** Explicit public URL returned by the media-hosting publish operation. */
  publishedUrl?: string;
  /** SHA-256 of the local bytes that were uploaded. */
  sha256?: string;
  novelWorkId?: string;
  novelChapterId?: string;
  documentKind?: string;
  documentId?: string;
  documentRevision?: number;
  chapterNo?: number;
  pageNo?: number;
  createdAt: number;
}

export interface LibraryImportEntry {
  entryType: "library_asset";
  asset: LibAsset;
  /** Explicit public URL returned by the media-hosting publish operation. */
  publishedUrl?: string;
  /** SHA-256 of the local bytes that were uploaded. */
  sha256?: string;
}

export type ImportEntry = LibraryImportEntry | CanonicalImportEntry;

export function libraryImportEntry(asset: LibAsset): LibraryImportEntry {
  return { entryType: "library_asset", asset };
}

function nonEmpty(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

/**
 * Only use material the user can inspect in the asset itself. In particular,
 * an analysis document is never silently substituted for a generation prompt.
 */
export function persistedGenerationPrompt(asset: LibAsset): string | null {
  const params = asset.params ?? {};
  const provenance = params.provenance && typeof params.provenance === "object"
    ? params.provenance as Record<string, unknown>
    : undefined;
  const visibleDocumentText = nonEmpty(getDocumentMeta(asset)?.text) ?? nonEmpty(params.text);
  // A document's generationInput describes how that document was produced,
  // whereas its visible body is the user-selected source material.
  if (asset.asset.kind === "text") return visibleDocumentText ?? nonEmpty(params.prompt) ?? null;
  return nonEmpty(provenance?.generationInput) ?? nonEmpty(params.prompt) ?? null;
}

export function importEntryPrompt(entry: ImportEntry): string | null {
  if (entry.entryType === "library_asset") return persistedGenerationPrompt(entry.asset);
  // A canonical image may expose document text as surrounding context. Its
  // persisted effectivePrompt is the only exact instruction used to render it.
  return entry.kind === "image"
    ? nonEmpty(entry.effectivePrompt) ?? null
    : nonEmpty(entry.text) ?? nonEmpty(entry.effectivePrompt) ?? null;
}

export function importEntryLabel(entry: ImportEntry): string {
  return entry.entryType === "library_asset" ? entry.asset.source : entry.title;
}

export function importEntryKind(entry: ImportEntry): "text" | "image" | "video" {
  return entry.entryType === "library_asset" ? entry.asset.asset.kind : entry.kind;
}

export function importEntryPublishedUrl(entry: ImportEntry): string | undefined {
  return nonEmpty(entry.publishedUrl);
}

export function importEntrySnapshot(entry: ImportEntry, text?: string, label?: string): SourceMaterialSnapshot {
  if (entry.entryType === "library_asset") return {
    ...snapshotAsset(entry.asset, text, label ?? entry.asset.source),
    ...(entry.publishedUrl ? { publishedUrl: entry.publishedUrl } : {}),
    ...(entry.sha256 ? { sha256: entry.sha256 } : {}),
  };
  return {
    kind: entry.kind,
    label: label ?? entry.title,
    source: entry.sourceUri,
    ...(entry.path ? { path: entry.path } : {}),
    ...(entry.publishedUrl ? { publishedUrl: entry.publishedUrl } : {}),
    ...(entry.sha256 ? { sha256: entry.sha256 } : {}),
    ...(text ?? entry.text ? { text: text ?? entry.text } : {}),
  };
}

export function importEntryAssetId(entry: ImportEntry): string | undefined {
  return entry.entryType === "library_asset" ? entry.asset.asset.id : undefined;
}

/** Stable selected order: page/shot ordering is supplied by the catalog before selection. */
export function createImportRecord(
  action: AssetImportRecord["action"],
  entries: readonly ImportEntry[],
  label: string,
): AssetImportRecord {
  return {
    action,
    assetIds: entries.flatMap((entry) => {
      const id = importEntryAssetId(entry);
      return id ? [id] : [];
    }),
    sourceMaterials: entries.map((entry) => importEntrySnapshot(
      entry,
      action === "reference" ? undefined : importEntryPrompt(entry) ?? undefined,
      label,
    )),
  };
}

export function mergeImportedPrompts(currentPrompt: string, entries: readonly ImportEntry[]): string {
  const blocks = entries.flatMap((entry, index) => {
    const prompt = importEntryPrompt(entry);
    return prompt ? [`【导入 ${index + 1} · ${importEntryLabel(entry)}】\n${prompt}`] : [];
  });
  if (!blocks.length) return currentPrompt;
  return [currentPrompt.trim(), ...blocks].filter(Boolean).join("\n\n---\n\n");
}

export function replaceImportedPrompt(entries: readonly ImportEntry[]): string | null {
  if (entries.length !== 1) return null;
  return importEntryPrompt(entries[0]);
}
