import { runNode, type RunNodeRequest } from "./ipc";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { useGenerationStore, type GenParams, type ImageGenerationReference } from "../store/useGenerationStore";
import { persistAssets, persistTask } from "./dbWrite";
import type { AssetRef } from "../types";
import { logEvent } from "./logger";
import { createId } from "./id";
import { createHistoryProvenance, snapshotAsset, type HistoryProvenance } from "./provenance";
import { catalogMetadata } from "./assetCatalog";

function importedSourceMaterials(params: GenParams) {
  return (params.importedSources ?? []).flatMap((record) => record.sourceMaterials);
}

function importedParentIds(params: GenParams) {
  return (params.importedSources ?? []).flatMap((record) => record.assetIds);
}

function uniqueMaterials<T>(materials: T[]): T[] {
  const seen = new Set<string>();
  return materials.filter((material) => {
    const key = JSON.stringify(material);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function sizeStr(size?: string) {
  if (!size) return "1024x1024";
  return size.split("(")[0].trim();
}

const REFERENCE_ROLE_LABELS: Record<ImageGenerationReference["role"], string> = {
  character_identity: "角色身份参考",
  outfit: "服装参考",
  style: "画风参考",
  pose: "姿势参考",
  scene: "场景参考",
  prop: "道具参考",
  previous_panel: "上一格连续性参考",
  base_image: "基础图参考",
  mask: "蒙版参考",
};

function normalizedReferences(params: GenParams): ImageGenerationReference[] {
  const configured = (params.references ?? []).filter((item) => item.path.trim().length > 0);
  if (configured.length > 0) return configured.map((item, index) => ({ ...item, path: item.path.trim(), sortOrder: item.sortOrder ?? index }));
  const legacy = params.referencePath.trim();
  return legacy ? [{ path: legacy, role: "base_image", weight: 1, sortOrder: 0 }] : [];
}

function assertFreshGeneratedAssetIds(assets: AssetRef[], preexistingAssetIds: ReadonlySet<string>): void {
  if (assets.length === 0) {
    throw new Error("图像服务没有返回图像产物，请重试。");
  }
  const returnedIds = new Set<string>();
  const invalid = assets.some((asset) => {
    const id = asset.id;
    if (!id || !id.trim() || preexistingAssetIds.has(id) || returnedIds.has(id)) return true;
    returnedIds.add(id);
    return false;
  });
  if (invalid) {
    throw new Error("图像服务返回无效或重复的资产 ID，已取消入库。");
  }
}

/** Generate an image via the active gateway config. Records project + params. */
export async function generateImage(
  params: GenParams,
  provenanceInput: Partial<Omit<HistoryProvenance, "schemaVersion" | "recordedAt">> = {},
): Promise<AssetRef[]> {
  const store = useLibraryStore.getState();
  const projectId = useProjectStore.getState().activeId ?? undefined;
  const references = normalizedReferences(params);
  const isImg2Img = references.length > 0;
  const label = isImg2Img ? "图生图" : "文生图";
  const taskId = createId("task");
  const nodeId = isImg2Img ? "gen_img2img" : "gen_txt2img";
  const createdAt = Date.now();
  // This snapshot is deliberately taken before provider dispatch: a provider
  // must never overwrite an existing local asset via INSERT OR REPLACE.
  const preexistingAssetIds = new Set(store.assets.map((asset) => asset.asset.id));
  const resolvedReferences = references.map((reference) => ({
    reference,
    asset: store.assets.find((item) => item.asset.path === reference.path),
  }));
  const defaultMaterials = resolvedReferences.map(({ reference, asset }) => asset
    ? snapshotAsset(asset, undefined, REFERENCE_ROLE_LABELS[reference.role])
    : { kind: "file" as const, label: REFERENCE_ROLE_LABELS[reference.role], path: reference.path });
  const provenance = createHistoryProvenance({
    originalInput: provenanceInput.originalInput ?? params.prompt,
    generationInput: provenanceInput.generationInput ?? params.prompt,
    systemInstruction: provenanceInput.systemInstruction,
    contextSnapshot: provenanceInput.contextSnapshot,
    sourceMaterials: uniqueMaterials([...defaultMaterials, ...importedSourceMaterials(params), ...(provenanceInput.sourceMaterials ?? [])]),
    parentAssetIds: [...new Set([
      ...resolvedReferences.flatMap(({ asset }) => asset ? [asset.asset.id] : []),
      ...importedParentIds(params),
      ...(provenanceInput.parentAssetIds ?? []),
    ])],
    comicGeneration: provenanceInput.comicGeneration,
    revision: provenanceInput.revision ?? { type: "generated" },
  });
  // Output classification belongs to this generation attempt. Never inherit a
  // selected input asset's catalog descriptor through a loaded form payload.
  const outputCatalog = provenanceInput.comicGeneration
    ? catalogMetadata("comic", "provider", { type: "comic_work", id: provenanceInput.comicGeneration.comicProjectId ?? provenanceInput.comicGeneration.comicRunId })
    : catalogMetadata("image_generation", "provider", { type: "generation_batch", id: taskId });
  const taskParams = {
    ...(params as unknown as Record<string, unknown>),
    ...outputCatalog,
    provenance,
  };
  const started = performance.now();
  logEvent("info", "generation.image.start", { taskId, nodeId, label, model: params.model, projectId, size: params.size, quality: params.quality, background: params.background, hasReference: isImg2Img, prompt: params.prompt });

  store.addTask({
    id: taskId,
    nodeId,
    label,
    model: params.model,
    projectId,
    status: "running",
    kind: "image",
    createdAt,
    params: taskParams,
  });

  const config: Record<string, unknown> = {
    prompt: params.prompt,
    size: sizeStr(params.size),
    quality: params.quality,
    background: params.background,
    referencePath: params.referencePath,
  };
  if ((params.references ?? []).length > 0) config.references = references;
  if (params.model) config.model = params.model;

  const req: RunNodeRequest = {
    nodeType: "textToImage",
    category: "generate",
    config,
    inputAssets: [],
  };

  // The nested provenance is the complete audit record. The two top-level
  // identifiers intentionally duplicate its immutable attempt link because
  // Rust validates them while atomically finishing/reconciling a comic run.
  // This object is used for the *first* persistAssets write, never patched in
  // later as a best-effort after a provider response.
  const comicGeneration = provenance.comicGeneration;
  const meta = {
    model: params.model,
    projectId,
    params: taskParams,
    ...(comicGeneration ? {
      comicRunId: comicGeneration.comicRunId,
      generationAttemptId: comicGeneration.generationAttemptId,
      comicGeneration: {
        comicRunId: comicGeneration.comicRunId,
        generationAttemptId: comicGeneration.generationAttemptId,
        kind: comicGeneration.kind,
        comicProjectId: comicGeneration.comicProjectId,
        pageNo: comicGeneration.pageNo,
        panelId: comicGeneration.panelId,
        panelNo: comicGeneration.panelNo,
        role: comicGeneration.role,
      },
    } : {}),
  };

  try {
    await persistTask({ id: taskId, nodeId, providerId: "zzone", status: "running", label, model: params.model, projectId, kind: "image", createdAt, params: taskParams });
    const result = await runNode(req);
    assertFreshGeneratedAssetIds(result.assets, preexistingAssetIds);
    const finishedAt = Date.now();
    await persistAssets(result.assets, label, meta);
    store.addAssets(result.assets, label, meta);
    store.updateTask(taskId, { status: "success", finishedAt });
    await persistTask({ id: taskId, nodeId, providerId: "zzone", status: "success", label, model: params.model, projectId, kind: "image", createdAt, finishedAt, params: taskParams })
      .catch((error) => logEvent("warn", "generation.image.success_task_persist_failed", { taskId, error: String(error) }));
    logEvent("info", "generation.image.end", { taskId, status: "success", durationMs: performance.now() - started, assetCount: result.assets.length });
    // reflect params in the form as-is (unchanged)
    return result.assets;
  } catch (e) {
    const msg = String(e);
    const finishedAt = Date.now();
    store.updateTask(taskId, { status: "error", finishedAt, error: msg });
    await persistTask({ id: taskId, nodeId, providerId: "zzone", status: "error", label, model: params.model, projectId, kind: "image", createdAt, finishedAt, error: msg, params: taskParams })
      .catch((persistError) => logEvent("error", "generation.image.error_persist_failed", { taskId, error: String(persistError) }));
    logEvent("error", "generation.image.end", { taskId, status: "error", durationMs: performance.now() - started, error: msg });
    throw e;
  }
}

/** Load an asset's params back into the shared generation form. */
export function applyAssetParams(params: Partial<GenParams> | undefined) {
  useGenerationStore.getState().load(params ?? {});
}
