import type { LibAsset } from "../store/useLibraryStore";

export type SourceMaterialKind = "text" | "image" | "video" | "file" | "context";
export type RevisionType = "generated" | "manual" | "ai_optimized" | "copy";

export interface SourceMaterialSnapshot {
  kind: SourceMaterialKind;
  label: string;
  assetId?: string;
  source?: string;
  path?: string;
  /** Public media-hosting URL explicitly published for a provider reference. */
  publishedUrl?: string;
  /** SHA-256 of the local bytes that were uploaded to the media host. */
  sha256?: string;
  text?: string;
  model?: string;
}

/** Immutable link between a newly persisted asset and the comic attempt that created it. */
export interface ComicGenerationProvenance {
  comicRunId: string;
  generationAttemptId: string;
  kind: "page" | "panel";
  comicProjectId?: string;
  pageNo?: number;
  panelId?: string;
  panelNo?: number;
  role?: string;
}

export interface HistoryProvenance {
  schemaVersion: 1;
  originalInput?: string;
  generationInput?: string;
  systemInstruction?: string;
  contextSnapshot?: string;
  sourceMaterials: SourceMaterialSnapshot[];
  parentAssetIds: string[];
  comicGeneration?: ComicGenerationProvenance;
  revision?: {
    type: RevisionType;
    instruction?: string;
    basedOnAssetId?: string;
  };
  recordedAt: number;
}

function parseComicGeneration(value: unknown): ComicGenerationProvenance | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const raw = value as Record<string, unknown>;
  const comicRunId = nonEmptyString(raw.comicRunId);
  const generationAttemptId = nonEmptyString(raw.generationAttemptId);
  const kind = raw.kind;
  if (!comicRunId || !generationAttemptId || (kind !== "page" && kind !== "panel")) return undefined;
  const positiveInteger = (item: unknown) => typeof item === "number" && Number.isSafeInteger(item) && item > 0 ? item : undefined;
  return {
    comicRunId,
    generationAttemptId,
    kind,
    comicProjectId: nonEmptyString(raw.comicProjectId),
    pageNo: positiveInteger(raw.pageNo),
    panelId: nonEmptyString(raw.panelId),
    panelNo: positiveInteger(raw.panelNo),
    role: nonEmptyString(raw.role),
  };
}

function nonEmptyString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

function stringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is string => typeof item === "string" && !!item.trim());
}

function parseSourceMaterials(value: unknown): SourceMaterialSnapshot[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((item) => {
    if (!item || typeof item !== "object") return [];
    const raw = item as Record<string, unknown>;
    const kind = raw.kind;
    const label = nonEmptyString(raw.label);
    if (!label || !["text", "image", "video", "file", "context"].includes(String(kind))) return [];
    return [{
      kind: kind as SourceMaterialKind,
      label,
      assetId: nonEmptyString(raw.assetId),
      source: nonEmptyString(raw.source),
      path: nonEmptyString(raw.path),
      publishedUrl: nonEmptyString(raw.publishedUrl),
      sha256: nonEmptyString(raw.sha256),
      text: nonEmptyString(raw.text),
      model: nonEmptyString(raw.model),
    }];
  });
}

function dedupeSourceMaterials(materials: SourceMaterialSnapshot[]): SourceMaterialSnapshot[] {
  const seen = new Set<string>();
  return materials.filter((material) => {
    const key = material.assetId
      ? `asset:${material.assetId}`
      : material.path
        ? `path:${material.path}`
        : material.publishedUrl
          ? `published:${material.publishedUrl}`
        : `content:${material.kind}:${material.label}:${material.text ?? ""}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

export function parseHistoryProvenance(value: unknown): HistoryProvenance | null {
  if (!value || typeof value !== "object") return null;
  const raw = value as Record<string, unknown>;
  if (raw.schemaVersion !== 1) return null;
  const revisionRaw = raw.revision && typeof raw.revision === "object"
    ? raw.revision as Record<string, unknown>
    : null;
  const revisionType = revisionRaw?.type;
  const revision = revisionType && ["generated", "manual", "ai_optimized", "copy"].includes(String(revisionType))
    ? {
        type: revisionType as RevisionType,
        instruction: nonEmptyString(revisionRaw?.instruction),
        basedOnAssetId: nonEmptyString(revisionRaw?.basedOnAssetId),
      }
    : undefined;
  return {
    schemaVersion: 1,
    originalInput: nonEmptyString(raw.originalInput),
    generationInput: nonEmptyString(raw.generationInput),
    systemInstruction: nonEmptyString(raw.systemInstruction),
    contextSnapshot: nonEmptyString(raw.contextSnapshot),
    sourceMaterials: dedupeSourceMaterials(parseSourceMaterials(raw.sourceMaterials)),
    parentAssetIds: stringArray(raw.parentAssetIds),
    comicGeneration: parseComicGeneration(raw.comicGeneration),
    revision,
    recordedAt: typeof raw.recordedAt === "number" && Number.isFinite(raw.recordedAt) ? raw.recordedAt : 0,
  };
}

export function historyProvenanceFromParams(params?: Record<string, unknown>): HistoryProvenance | null {
  return parseHistoryProvenance(params?.provenance);
}

export function createHistoryProvenance(input: Partial<Omit<HistoryProvenance, "schemaVersion" | "recordedAt">> = {}): HistoryProvenance {
  return {
    schemaVersion: 1,
    originalInput: nonEmptyString(input.originalInput),
    generationInput: nonEmptyString(input.generationInput),
    systemInstruction: nonEmptyString(input.systemInstruction),
    contextSnapshot: nonEmptyString(input.contextSnapshot),
    sourceMaterials: dedupeSourceMaterials(parseSourceMaterials(input.sourceMaterials)),
    parentAssetIds: stringArray(input.parentAssetIds),
    comicGeneration: parseComicGeneration(input.comicGeneration),
    revision: input.revision,
    recordedAt: Date.now(),
  };
}

export function inheritHistoryProvenance(
  parent: HistoryProvenance | null,
  next: Partial<Omit<HistoryProvenance, "schemaVersion" | "recordedAt">> = {},
): HistoryProvenance {
  return createHistoryProvenance({
    originalInput: next.originalInput ?? parent?.originalInput,
    generationInput: next.generationInput ?? parent?.generationInput,
    systemInstruction: next.systemInstruction ?? parent?.systemInstruction,
    contextSnapshot: next.contextSnapshot ?? parent?.contextSnapshot,
    sourceMaterials: next.sourceMaterials ?? parent?.sourceMaterials ?? [],
    parentAssetIds: next.parentAssetIds ?? parent?.parentAssetIds ?? [],
    comicGeneration: next.comicGeneration ?? parent?.comicGeneration,
    revision: next.revision,
  });
}

export function snapshotAsset(asset: LibAsset, text?: string, label?: string): SourceMaterialSnapshot {
  return {
    kind: asset.asset.kind,
    label: label || asset.source,
    assetId: asset.asset.id,
    source: asset.source,
    path: asset.asset.path,
    text: nonEmptyString(text),
    model: asset.model,
  };
}

/** Compatibility view for media records created before source snapshots existed. */
export function legacyProvenanceFromAsset(asset: LibAsset): HistoryProvenance | null {
  const params = asset.params ?? {};
  const nested = params.params && typeof params.params === "object"
    ? params.params as Record<string, unknown>
    : {};
  const prompt = nonEmptyString(params.prompt) ?? nonEmptyString(nested.prompt);
  const referencePath = nonEmptyString(params.referencePath) ?? nonEmptyString(nested.referencePath);
  if (!prompt && !referencePath) return null;
  return createHistoryProvenance({
    originalInput: prompt,
    generationInput: prompt,
    sourceMaterials: referencePath ? [{ kind: "file", label: "参考文件", path: referencePath }] : [],
    parentAssetIds: [],
  });
}
