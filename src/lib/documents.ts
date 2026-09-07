import type { StoryboardShot } from "./video/storyboard";
import { parseStoryboardShots } from "./video/storyboard";
import { createId } from "./id";
import { saveDocumentVersionAtomic } from "./ipc";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import {
  historyProvenanceFromParams,
  inheritHistoryProvenance,
  type HistoryProvenance,
} from "./provenance";

export type DocumentType =
  | "director"
  | "script"
  | "storyboard"
  | "consistency"
  | "qc"
  | "anchor"
  | "orchestration"
  | "pipeline"
  | "novel"
  | "document";

export type DocumentChangeType = "generated" | "manual" | "ai_optimized" | "copy";

export interface DocumentMeta {
  text: string;
  title: string;
  documentType: DocumentType;
  documentId: string;
  version: number;
  parentAssetId?: string;
  changeType: DocumentChangeType;
  agentId?: string;
  shots?: StoryboardShot[];
  provenance?: HistoryProvenance;
}

const TYPE_LABELS: Record<DocumentType, string> = {
  director: "导演规划",
  script: "剧本",
  storyboard: "分镜",
  consistency: "一致性",
  qc: "质检",
  anchor: "角色锚",
  orchestration: "Agent 编排",
  pipeline: "完整流水线",
  novel: "小说章节",
  document: "文档",
};

const CHANGE_LABELS: Record<DocumentChangeType, string> = {
  generated: "自动生成",
  manual: "手动修改",
  ai_optimized: "智能优化",
  copy: "保存副本",
};

const DOCUMENT_TYPES = new Set<DocumentType>([
  "director", "script", "storyboard", "consistency", "qc", "anchor", "orchestration", "pipeline", "novel", "document",
]);

export function documentTypeLabel(type: DocumentType): string {
  return TYPE_LABELS[type];
}

export function documentChangeLabel(type: DocumentChangeType): string {
  return CHANGE_LABELS[type];
}

export function inferDocumentType(source: string): DocumentType {
  const value = source.toLowerCase();
  if (source.includes("分镜") || value.includes("storyboard")) return "storyboard";
  if (source.includes("剧本") || source.includes("编剧") || value.includes("script")) return "script";
  if (source.includes("导演")) return "director";
  if (source.includes("一致性")) return "consistency";
  if (source.includes("质检")) return "qc";
  if (source.includes("锚")) return "anchor";
  if (source.includes("编排")) return "orchestration";
  if (source.includes("流水线")) return "pipeline";
  if (source.includes("小说") || source.includes("原著")) return "novel";
  return "document";
}

export function getDocumentMeta(asset: LibAsset): DocumentMeta | null {
  if (asset.asset.kind !== "text") return null;
  const params = asset.params ?? {};
  const text = typeof params.text === "string" ? params.text : "";
  const documentType = typeof params.documentType === "string" && DOCUMENT_TYPES.has(params.documentType as DocumentType)
    ? params.documentType as DocumentType
    : inferDocumentType(asset.source);
  const documentId = typeof params.documentId === "string" && params.documentId
    ? params.documentId
    : asset.asset.id;
  const version = typeof params.version === "number" && Number.isFinite(params.version)
    ? Math.max(1, Math.floor(params.version))
    : 1;
  const shots = documentType === "storyboard" ? parseStoryboardShots(text) : undefined;
  return {
    text,
    title: typeof params.title === "string" && params.title.trim() ? params.title.trim() : asset.source,
    documentType,
    documentId,
    version,
    parentAssetId: typeof params.parentAssetId === "string" ? params.parentAssetId : undefined,
    changeType: typeof params.changeType === "string" ? params.changeType as DocumentChangeType : "generated",
    agentId: typeof params.agentId === "string" ? params.agentId : undefined,
    shots,
    provenance: historyProvenanceFromParams(params) ?? undefined,
  };
}

export function getDocumentVersions(asset: LibAsset, allAssets = useLibraryStore.getState().assets): LibAsset[] {
  const meta = getDocumentMeta(asset);
  if (!meta) return [];
  return allAssets
    .filter((candidate) => getDocumentMeta(candidate)?.documentId === meta.documentId)
    .sort((a, b) => (getDocumentMeta(b)?.version ?? 0) - (getDocumentMeta(a)?.version ?? 0));
}

export function getDocumentDisplayVersion(asset: LibAsset, allAssets = useLibraryStore.getState().assets): number {
  const params = asset.params ?? {};
  if (typeof params.version === "number" && Number.isFinite(params.version)) {
    return Math.max(1, Math.floor(params.version));
  }
  const versions = getDocumentVersions(asset, allAssets).sort((a, b) => a.createdAt - b.createdAt);
  const index = versions.findIndex((candidate) => candidate.asset.id === asset.asset.id);
  return index >= 0 ? index + 1 : 1;
}

export async function saveDocumentVersion(input: {
  title: string;
  text: string;
  model?: string;
  projectId?: string;
  documentType: DocumentType;
  parent?: LibAsset;
  changeType: DocumentChangeType;
  agentId?: string;
  provenance?: Partial<Omit<HistoryProvenance, "schemaVersion" | "recordedAt">>;
  revisionInstruction?: string;
  metadata?: Record<string, unknown>;
  expectedHeadAssetId?: string;
  allowBranch?: boolean;
}): Promise<LibAsset> {
  const parentMeta = input.parent ? getDocumentMeta(input.parent) : null;
  const documentId = parentMeta?.documentId ?? createId("document");
  const lockKey = parentMeta?.documentId ?? `${input.projectId ?? ""}:${input.agentId ?? input.documentType}:${String(input.metadata?.videoWorkflowId ?? "")}`;
  if (documentSaveLocks.has(lockKey)) throw new Error("当前文档正在保存，请等待完成后再试");
  documentSaveLocks.add(lockKey);
  try {
  const currentHead = useLibraryStore.getState().assets
    .filter((asset) => asset.params?.videoBranch !== true && getDocumentMeta(asset)?.documentId === documentId)
    .sort((left, right) => right.createdAt - left.createdAt)[0];
  if (!input.allowBranch && input.expectedHeadAssetId && currentHead?.asset.id !== input.expectedHeadAssetId) {
    throw new Error("版本冲突：当前生产版已变化。请创建历史分支，或迁移到最新版本后再保存");
  }
  const title = input.title.trim() || documentTypeLabel(input.documentType);
  const text = input.text.trim();
  if (!text) throw new Error("文档内容不能为空");

  const provenance = inheritHistoryProvenance(parentMeta?.provenance ?? null, {
    ...input.provenance,
    revision: {
      type: input.changeType,
      instruction: input.revisionInstruction,
      basedOnAssetId: input.parent?.asset.id,
    },
  });
  const params: Record<string, unknown> = {
    ...(input.metadata ?? {}),
    text,
    title,
    documentType: input.documentType,
    documentId,
    parentAssetId: input.parent?.asset.id,
    changeType: input.changeType,
    agentId: input.agentId,
    provenance,
    updatedAt: Date.now(),
  };
  const result = await saveDocumentVersionAtomic({
    label: title,
    text,
    model: input.model,
    projectId: input.projectId,
    documentId,
    params,
    expectedHeadAssetId: input.allowBranch ? undefined : input.expectedHeadAssetId ?? input.parent?.asset.id,
    allowBranch: input.allowBranch,
  });
  const asset = result.asset;
  const savedParams = result.params;
  const source = title;
  const meta = { model: input.model, projectId: input.projectId, params: savedParams };
  const saved: LibAsset = {
    asset,
    source,
    model: input.model,
    projectId: input.projectId,
    params: savedParams,
    createdAt: Date.now(),
  };
  useLibraryStore.getState().addAssets([asset], source, meta);
  return saved;
  } finally {
    documentSaveLocks.delete(lockKey);
  }
}

const documentSaveLocks = new Set<string>();
