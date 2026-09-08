import type { LibAsset } from "../store/useLibraryStore";

/** A stable product-facing classification for the shared asset history. */
export type AssetCatalogCategory = "image_generation" | "video_generation" | "novel" | "comic" | "short_drama" | "upload" | "legacy";
export type AssetCatalogOrigin = "provider" | "local_upload" | "system_mirror" | "manual_concat" | "workspace" | "legacy";
export type AssetCatalogGroupType = "generation_batch" | "video_shots" | "novel_chapter" | "comic_work" | "comic_chapter" | "short_drama_workflow" | "upload_batch";

export interface AssetCatalogGroup {
  type: AssetCatalogGroupType;
  id: string;
  label?: string;
}

export interface AssetCatalogDescriptor {
  version: 1;
  category: Exclude<AssetCatalogCategory, "legacy">;
  origin: Exclude<AssetCatalogOrigin, "legacy">;
  group?: AssetCatalogGroup;
}

export interface AssetCatalogClassification {
  category: AssetCatalogCategory;
  origin: AssetCatalogOrigin;
  group?: AssetCatalogGroup;
  isLegacy: boolean;
}

export interface LibraryCatalogEntry {
  entryType: "library_asset";
  asset: LibAsset;
  classification: AssetCatalogClassification;
  canDelete: true;
}

/** A comic-workspace record which is intentionally not a row in the generic assets table. */
export interface ComicWorkspaceCatalogEntry {
  entryType: "comic_workspace";
  readonly: true;
  sourceUri: string;
  projectId: string;
  kind: "text" | "image";
  title: string;
  text?: string;
  sourcePrompt?: string;
  effectivePrompt?: string;
  promptSnapshotComplete: boolean;
  path?: string;
  novelWorkId: string;
  novelChapterId?: string;
  chapterNo?: number;
  chapterTitle?: string;
  documentId?: string;
  documentRevision?: number;
  documentKind?: string;
  pageNo?: number;
  createdAt: number;
  stale: boolean;
  classification: AssetCatalogClassification & { category: "comic" };
  canDelete: false;
}

export type AssetCatalogEntry = LibraryCatalogEntry | ComicWorkspaceCatalogEntry;

function record(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : undefined;
}

function text(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

function catalogGroup(value: unknown): AssetCatalogGroup | undefined {
  const group = record(value);
  const type = text(group?.type);
  const id = text(group?.id);
  const validTypes: AssetCatalogGroupType[] = ["generation_batch", "video_shots", "novel_chapter", "comic_work", "comic_chapter", "short_drama_workflow", "upload_batch"];
  if (!type || !id || !validTypes.includes(type as AssetCatalogGroupType)) return undefined;
  return { type: type as AssetCatalogGroupType, id, ...(text(group?.label) ? { label: text(group?.label) } : {}) };
}

function explicitDescriptor(params: Record<string, unknown>): AssetCatalogDescriptor | undefined {
  const descriptor = record(params.catalog);
  const category = text(descriptor?.category);
  const origin = text(descriptor?.origin);
  const categories: Exclude<AssetCatalogCategory, "legacy">[] = ["image_generation", "video_generation", "novel", "comic", "short_drama", "upload"];
  const origins: Exclude<AssetCatalogOrigin, "legacy">[] = ["provider", "local_upload", "system_mirror", "manual_concat", "workspace"];
  if (descriptor?.version !== 1 || !category || !origin || !categories.includes(category as Exclude<AssetCatalogCategory, "legacy">) || !origins.includes(origin as Exclude<AssetCatalogOrigin, "legacy">)) return undefined;
  return { version: 1, category: category as AssetCatalogDescriptor["category"], origin: origin as AssetCatalogDescriptor["origin"], ...(catalogGroup(descriptor.group) ? { group: catalogGroup(descriptor.group) } : {}) };
}

function fallbackGroup(type: AssetCatalogGroupType, id: unknown, label?: string): AssetCatalogGroup | undefined {
  const value = text(id);
  return value ? { type, id: value, ...(label ? { label } : {}) } : undefined;
}

/**
 * Classifies only explicit catalog data or domain identifiers whose semantics already
 * exist in persisted records. Display labels are deliberately never used as evidence.
 */
export function classifyAssetCatalog(asset: LibAsset): AssetCatalogClassification {
  const params = asset.params ?? {};
  const explicit = explicitDescriptor(params);
  if (explicit) return { category: explicit.category, origin: explicit.origin, ...(explicit.group ? { group: explicit.group } : {}), isLegacy: false };

  const workflowId = text(params.videoWorkflowId);
  // These two historic upload paths predate catalog metadata. Their booleans
  // are persisted contracts, unlike their display labels.
  if (params.comicStyleReference === true) {
    const group = fallbackGroup("comic_work", params.novelWorkId);
    return { category: "upload", origin: "local_upload", ...(group ? { group } : {}), isLegacy: false };
  }
  if (params.videoWorkReference === true) {
    return { category: "upload", origin: "local_upload", ...(workflowId ? { group: { type: "short_drama_workflow", id: workflowId } } : {}), isLegacy: false };
  }

  // A short-drama stage can carry a novel chapter lineage. The workflow is its
  // product ownership and must take precedence over that inherited source.
  if (workflowId) {
    return { category: "short_drama", origin: "workspace", group: { type: "short_drama_workflow", id: workflowId }, isLegacy: false };
  }

  const comicGeneration = record(params.comicGeneration);
  if (comicGeneration) {
    const workId = comicGeneration?.novelWorkId ?? params.novelWorkId;
    const chapterId = comicGeneration?.novelChapterId;
    const group = chapterId
      ? fallbackGroup("comic_chapter", `${String(workId ?? "")}::${String(chapterId)}`)
      : fallbackGroup("comic_work", workId);
    return { category: "comic", origin: "provider", ...(group ? { group } : {}), isLegacy: false };
  }

  const revisionId = text(params.novelChapterRevisionId);
  const isNovelMirror = (revisionId || params.sourceKind === "novel_chapter")
    && (params.documentType === "novel" || params.agentId === "novel_source");
  if (isNovelMirror) {
    const group = fallbackGroup("novel_chapter", `${String(params.novelWorkId ?? "")}::${String(params.novelChapterId ?? revisionId ?? "")}`);
    return { category: "novel", origin: "system_mirror", ...(group ? { group } : {}), isLegacy: false };
  }

  const shotGroupId = text(params.shotGroupId);
  if (shotGroupId) {
    return { category: "video_generation", origin: params.sourceAssetIds ? "manual_concat" : "provider", group: { type: "video_shots", id: shotGroupId }, isLegacy: false };
  }

  if (asset.asset.kind === "image" && (typeof params.prompt === "string" || typeof record(params.params)?.prompt === "string")) {
    return { category: "image_generation", origin: "provider", isLegacy: false };
  }
  return { category: "legacy", origin: "legacy", isLegacy: true };
}

export function assetMatchesCatalogCategory(asset: LibAsset, category: AssetCatalogCategory | "all"): boolean {
  return category === "all" || classifyAssetCatalog(asset).category === category;
}

export function catalogMetadata(category: AssetCatalogDescriptor["category"], origin: AssetCatalogDescriptor["origin"], group?: AssetCatalogGroup): { catalog: AssetCatalogDescriptor } {
  return { catalog: { version: 1, category, origin, ...(group ? { group } : {}) } };
}

export interface AssetCatalogGroupBucket {
  id: string;
  group?: AssetCatalogGroup;
  assets: LibraryCatalogEntry[];
}

export function groupCatalogAssets(assets: LibAsset[]): AssetCatalogGroupBucket[] {
  const buckets = new Map<string, AssetCatalogGroupBucket>();
  for (const asset of assets) {
    const classification = classifyAssetCatalog(asset);
    // A provider can persist a one-image generation batch for every click. Those
    // batches still retain their metadata on each asset, but are displayed in
    // the ordinary visual grid instead of as rows of one card.
    const displayGroup = classification.group?.type === "generation_batch" ? undefined : classification.group;
    // Keep ordinary history in one grid. A card-sized bucket per ungrouped asset
    // makes the page look fragmented and hides the useful chronological scan.
    const id = displayGroup
      ? `${asset.projectId ?? "unassigned"}:${displayGroup.type}:${displayGroup.id}`
      : `ungrouped:${asset.projectId ?? "unassigned"}`;
    const bucket = buckets.get(id) ?? { id, ...(displayGroup ? { group: displayGroup } : {}), assets: [] };
    bucket.assets.push({ entryType: "library_asset", asset, classification, canDelete: true });
    buckets.set(id, bucket);
  }
  return [...buckets.values()].sort((left, right) => Math.max(...right.assets.map((item) => item.asset.createdAt)) - Math.max(...left.assets.map((item) => item.asset.createdAt)));
}
