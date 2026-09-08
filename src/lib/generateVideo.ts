import { convertFileSrc } from "@tauri-apps/api/core";
import { runVideo, saveMediaAsset, type AssetRef, type RunNodeRequest } from "./ipc";
import { concatMp4 } from "./media";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { productionManifestMismatch, type VideoLocalImageReference, type VideoParams } from "../store/useVideoStore";
import { persistAssets, persistTask } from "./dbWrite";
import { logEvent } from "./logger";
import { createId } from "./id";
import { createHistoryProvenance, snapshotAsset, type HistoryProvenance, type SourceMaterialSnapshot } from "./provenance";
import { isPublicHttpsUrl } from "./video/referenceUrl";
import { catalogMetadata } from "./assetCatalog";

function importedSourceMaterials(params: VideoParams, shotId?: string) {
  return (params.importedSources ?? [])
    .filter((record) => !shotId || !record.targetShotId || record.targetShotId === shotId)
    .flatMap((record) => record.sourceMaterials);
}

function importedParentIds(params: VideoParams, shotId?: string) {
  return (params.importedSources ?? [])
    .filter((record) => !shotId || !record.targetShotId || record.targetShotId === shotId)
    .flatMap((record) => record.assetIds);
}

type NativeLocalImage = Pick<VideoLocalImageReference, "assetId" | "sourceUri">;
type HistoryLocalImage = VideoLocalImageReference;

function localImageIdentity(reference: VideoLocalImageReference): NativeLocalImage {
  if (!reference.assetId && !reference.sourceUri) throw new Error("本地图片参考缺少受控资产身份");
  return {
    ...(reference.assetId ? { assetId: reference.assetId } : {}),
    ...(reference.sourceUri ? { sourceUri: reference.sourceUri } : {}),
  };
}

function historyLocalImage(reference: VideoLocalImageReference): HistoryLocalImage {
  return { ...localImageIdentity(reference), path: reference.path, label: reference.label };
}

function localImageMaterials(shotNo: number, references: VideoLocalImageReference[]): SourceMaterialSnapshot[] {
  return references.map((reference, index) => ({
    kind: "image" as const,
    label: `第 ${shotNo} 镜本地参考图 ${index + 1} · ${reference.label}`,
    ...(reference.assetId ? { assetId: reference.assetId } : {}),
    ...(reference.sourceUri ? { source: reference.sourceUri } : {}),
    path: reference.path,
  }));
}

function validateLocalImages(
  references: VideoLocalImageReference[],
  projectId: string,
  assets: LibAsset[],
) {
  const seen = new Set<string>();
  for (const reference of references) {
    const identity = localImageIdentity(reference);
    const key = identity.assetId ? `asset:${identity.assetId}` : `comic:${identity.sourceUri}`;
    if (seen.has(key)) throw new Error(`本地图片参考重复：${reference.label}`);
    seen.add(key);
    if (identity.assetId) {
      const asset = assets.find((item) => item.asset.id === identity.assetId);
      if (!asset || asset.projectId !== projectId || asset.asset.kind !== "image") {
        throw new Error(`本地图片参考不属于当前项目或不是图片资产：${reference.label}`);
      }
    }
    if (identity.sourceUri && !identity.sourceUri.startsWith("comic-md://")) {
      throw new Error(`本地图片参考来源身份无效：${reference.label}`);
    }
  }
}

function validateShotReferences(
  shotNo: number,
  mode: "text" | "first_frame" | "reference",
  imageCount: number,
  localImageCount: number,
  videoCount: number,
) {
  if (mode === "text" && imageCount + localImageCount + videoCount > 0) {
    throw new Error(`第 ${shotNo} 镜为 text 模式，不能携带逐镜参考素材`);
  }
  if (mode === "first_frame" && (imageCount + localImageCount !== 1 || videoCount > 0)) {
    throw new Error(`第 ${shotNo} 镜的 first_frame 模式需要且只能使用 1 张图片`);
  }
  if (mode === "reference" && imageCount + localImageCount + videoCount === 0) {
    throw new Error(`第 ${shotNo} 镜的 reference 模式缺少参考素材`);
  }
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

function orderedForConcat(assets: LibAsset[]): LibAsset[] {
  return [...assets].sort((a, b) => {
    const aGroup = String(a.params?.shotGroupId ?? "");
    const bGroup = String(b.params?.shotGroupId ?? "");
    if (aGroup && aGroup === bGroup) {
      return Number(a.params?.shotNo ?? 0) - Number(b.params?.shotNo ?? 0);
    }
    return a.createdAt - b.createdAt;
  });
}

export async function generateVideo(
  params: VideoParams,
  provenanceInput: Partial<Omit<HistoryProvenance, "schemaVersion" | "recordedAt">> = {},
): Promise<AssetRef[]> {
  const manifestIssue = productionManifestMismatch(params);
  if (manifestIssue) throw new Error(`已审查生产条件失效：${manifestIssue}`);
  const store = useLibraryStore.getState();
  const projectId = useProjectStore.getState().activeId ?? undefined;
  if (!projectId) throw new Error("请先选择当前项目，再生成视频");
  const taskId = createId("task");
  const shotGroupId = createId("video_shots");
  const nodeId = "gen_video_shots";
  const createdAt = Date.now();
  const shots = params.shots.map((shot, index) => ({ ...shot, shotNo: index + 1, prompt: shot.prompt.trim() })).filter((shot) => shot.prompt);
  if (!shots.length) throw new Error("至少需要一个视频镜头 Prompt");

  const images = [...new Set(params.images.map((url) => url.trim()).filter(Boolean))];
  const videos = [...new Set((params.videos ?? []).map((url) => url.trim()).filter(Boolean))];
  const audios = [...new Set((params.audios ?? []).map((url) => url.trim()).filter(Boolean))];
  if (audios.length) throw new Error("当前视频工作区暂不处理声音或音频参考");
  const mode = params.mode ?? (images.length ? "first_frame" : "text");
  const perShotUrls = shots.flatMap((shot) => [...(shot.referenceImages ?? []), ...(shot.referenceVideos ?? [])]);
  const invalidUrl = [...images, ...videos, ...perShotUrls].find((url) => !isPublicHttpsUrl(url));
  if (invalidUrl) throw new Error(`视频参考素材必须是无凭据的公网 HTTPS URL，不能使用本机或内网地址：${invalidUrl}`);
  for (const shot of shots) {
    const shotLocalImages = shot.referenceLocalImages ?? [];
    const shotImages = shot.referenceImages?.length || shotLocalImages.length ? [...new Set(shot.referenceImages ?? [])] : images;
    const shotVideos = shot.referenceVideos?.length ? [...new Set(shot.referenceVideos)] : videos;
    validateLocalImages(shotLocalImages, projectId, store.assets);
    validateShotReferences(shot.shotNo, shot.referenceStrategy ?? mode, shotImages.length, shotLocalImages.length, shotVideos.length);
  }
  const label = `视频镜头 × ${shots.length}`;
  const defaultMaterials: SourceMaterialSnapshot[] = [];
  const storyboardSource = params.storyboardSourceAssetId
    ? store.assets.find((item) => item.asset.id === params.storyboardSourceAssetId)
    : undefined;
  if (storyboardSource) defaultMaterials.push(snapshotAsset(storyboardSource, undefined, "视频分镜来源"));
  images.forEach((path, index) => defaultMaterials.push({ kind: "file", label: `视频参考图 ${index + 1}`, path }));
  videos.forEach((path, index) => defaultMaterials.push({ kind: "file", label: `视频参考视频 ${index + 1}`, path }));
  const persistedShots = shots.map(({ referenceLocalImages, ...shot }) => ({
    ...shot,
    ...(referenceLocalImages?.length ? { referenceLocalImages: referenceLocalImages.map(historyLocalImage) } : {}),
  }));
  const workflowId = typeof storyboardSource?.params?.videoWorkflowId === "string" && storyboardSource.params.videoWorkflowId.trim()
    ? storyboardSource.params.videoWorkflowId
    : undefined;
  const outputCatalog = params.productionManifest
    ? catalogMetadata("short_drama", "workspace", { type: "short_drama_workflow", id: workflowId ?? params.productionManifest.storyboardAssetId })
    : catalogMetadata("video_generation", "provider", { type: "video_shots", id: shotGroupId });
  const commonParams = {
    ...params,
    ...outputCatalog,
    shots: persistedShots,
    shotGroupId,
    shotCount: shots.length,
  };
  const taskParams = {
    ...commonParams,
    provenance: createHistoryProvenance({
      originalInput: provenanceInput.originalInput ?? shots.map((shot) => shot.prompt).join("\n\n---\n\n"),
      generationInput: provenanceInput.generationInput ?? shots.map((shot) => shot.prompt).join("\n\n---\n\n"),
      systemInstruction: provenanceInput.systemInstruction,
      contextSnapshot: provenanceInput.contextSnapshot,
      sourceMaterials: uniqueMaterials([...defaultMaterials, ...shots.flatMap((shot) => localImageMaterials(shot.shotNo, shot.referenceLocalImages ?? [])), ...importedSourceMaterials(params), ...(provenanceInput.sourceMaterials ?? [])]),
      parentAssetIds: [...new Set([...(storyboardSource ? [storyboardSource.asset.id] : []), ...shots.flatMap((shot) => (shot.referenceLocalImages ?? []).flatMap((reference) => reference.assetId ? [reference.assetId] : [])), ...importedParentIds(params), ...(provenanceInput.parentAssetIds ?? [])])],
      revision: provenanceInput.revision ?? { type: "generated" },
    }),
  };
  const started = performance.now();
  const generated: AssetRef[] = [];

  store.addTask({ id: taskId, nodeId, label, model: params.model, projectId, kind: "video", status: "running", createdAt, params: taskParams });
  await persistTask({ id: taskId, nodeId, providerId: "zzone", status: "running", label, model: params.model, projectId, kind: "video", createdAt, params: taskParams });

  try {
    for (let index = 0; index < shots.length; index += 1) {
      const shot = shots[index];
      const shotLocalImages = shot.referenceLocalImages ?? [];
      const shotImages = shot.referenceImages?.length || shotLocalImages.length ? [...new Set(shot.referenceImages ?? [])] : images;
      const shotVideos = shot.referenceVideos?.length ? [...new Set(shot.referenceVideos)] : videos;
      const shotMode = shot.referenceStrategy ?? mode;
      validateShotReferences(shot.shotNo, shotMode, shotImages.length, shotLocalImages.length, shotVideos.length);
      const config = {
        prompt: shot.prompt,
        duration_s: shot.durationS,
        model: params.model,
        aspect_ratio: params.aspectRatio,
        resolution: params.resolution,
        mode: shotMode,
        images: shotMode === "text" ? [] : shotImages,
        local_images: shotMode === "text" ? [] : shotLocalImages.map(localImageIdentity),
        videos: shotMode === "text" ? [] : shotVideos,
        audios: [],
        project_id: projectId,
      };
      const req: RunNodeRequest = { nodeType: "textToVideo", category: "video", config, inputAssets: [] };
      logEvent("info", "generation.video.shot_start", { taskId, shotGroupId, shotNo: shot.shotNo, shotCount: shots.length, durationS: shot.durationS, model: params.model });
      const result = await runVideo(req);
      if (!result.assets.length) throw new Error(`第 ${shot.shotNo} 镜接口未返回视频资源`);

      for (let outputIndex = 0; outputIndex < result.assets.length; outputIndex += 1) {
        const asset = result.assets[outputIndex];
        const providerTaskId = asset.id.startsWith("zzone:") ? asset.id.slice("zzone:".length) : undefined;
        const shotReferenceMaterials: SourceMaterialSnapshot[] = [
          ...shotImages.map((path, referenceIndex) => ({ kind: "file" as const, label: `第 ${shot.shotNo} 镜参考图 ${referenceIndex + 1}`, path })),
          ...localImageMaterials(shot.shotNo, shotLocalImages),
          ...shotVideos.map((path, referenceIndex) => ({ kind: "file" as const, label: `第 ${shot.shotNo} 镜参考视频 ${referenceIndex + 1}`, path })),
        ];
        const shotParams = {
          ...commonParams,
          prompt: shot.prompt,
          shotNo: shot.shotNo,
          shotDurationS: shot.durationS,
          anchorIds: shot.anchorIds ?? [],
          continuityFrom: shot.continuityFrom ?? null,
          referenceStrategy: shotMode,
          referenceAssetIds: shot.referenceAssetIds ?? [],
          referenceImages: shotImages,
          ...(shotLocalImages.length ? { referenceLocalImages: shotLocalImages.map(historyLocalImage) } : {}),
          referenceVideos: shotVideos,
          providerTaskId,
          shotOutputIndex: outputIndex + 1,
          provenance: createHistoryProvenance({
            originalInput: provenanceInput.originalInput ?? shots.map((item) => item.prompt).join("\n\n---\n\n"),
            generationInput: shot.prompt,
            sourceMaterials: uniqueMaterials([...(shot.referenceImages?.length || shot.referenceVideos?.length || shotLocalImages.length ? shotReferenceMaterials : defaultMaterials), ...importedSourceMaterials(params, shot.id), ...(provenanceInput.sourceMaterials ?? [])]),
            parentAssetIds: [...new Set([...(storyboardSource ? [storyboardSource.asset.id] : []), ...shotLocalImages.flatMap((reference) => reference.assetId ? [reference.assetId] : []), ...importedParentIds(params, shot.id), ...(provenanceInput.parentAssetIds ?? [])])],
            revision: provenanceInput.revision ?? { type: "generated" },
          }),
        };
        const source = `视频镜头 ${shot.shotNo}/${shots.length}`;
        const meta = { model: params.model, projectId, params: shotParams };
        await persistAssets([asset], source, meta);
        store.addAssets([asset], source, meta);
        generated.push(asset);
      }
      logEvent("info", "generation.video.shot_end", { taskId, shotGroupId, shotNo: shot.shotNo, assetCount: result.assets.length });
    }

    const finishedAt = Date.now();
    await persistTask({ id: taskId, nodeId, providerId: "zzone", status: "success", label, model: params.model, projectId, kind: "video", createdAt, finishedAt, params: taskParams });
    store.updateTask(taskId, { status: "success", finishedAt });
    logEvent("info", "generation.video.end", { taskId, status: "success", durationMs: performance.now() - started, assetCount: generated.length, shotGroupId });
    return generated;
  } catch (error) {
    const message = String(error);
    const finishedAt = Date.now();
    store.updateTask(taskId, { status: "error", finishedAt, error: message });
    await persistTask({ id: taskId, nodeId, providerId: "zzone", status: "error", label, model: params.model, projectId, kind: "video", createdAt, finishedAt, error: message, params: taskParams })
      .catch((persistError) => logEvent("error", "generation.video.error_persist_failed", { taskId, error: String(persistError) }));
    logEvent("error", "generation.video.end", { taskId, status: "error", durationMs: performance.now() - started, error: message, completedAssets: generated.length });
    throw error;
  }
}

export async function concatVideoAssets(assets: LibAsset[]): Promise<AssetRef> {
  if (assets.length < 2) throw new Error("至少选择两个视频镜头才能拼接");
  const ordered = orderedForConcat(assets);
  if (ordered.some((item) => item.asset.kind !== "video")) throw new Error("只能拼接视频资源");
  const projectId = useProjectStore.getState().activeId ?? undefined;
  if (ordered.some((item) => item.projectId !== projectId)) throw new Error("不能跨项目拼接视频资源");

  const blobs = await Promise.all(ordered.map(async (item) => {
    const response = await fetch(convertFileSrc(item.asset.path));
    if (!response.ok) throw new Error(`读取视频镜头失败：${response.status}`);
    return response.blob();
  }));
  const output = await concatMp4(blobs);
  const saved = await saveMediaAsset("video", "mp4", new Uint8Array(await output.arrayBuffer()));
  const asset: AssetRef = {
    ...saved,
    durationS: ordered.reduce((sum, item) => sum + (item.asset.durationS ?? Number(item.params?.shotDurationS ?? 0)), 0),
  };
  const sourceAssetIds = ordered.map((item) => item.asset.id);
  const params = {
    concatenated: true,
    sourceAssetIds,
    shotCount: ordered.length,
    duration_s: asset.durationS,
    shotGroupId: createId("video_concat"),
  };
  const model = [...new Set(ordered.map((item) => item.model).filter(Boolean))].join(" + ") || "本地拼接";
  const meta = { model, projectId, params };
  await persistAssets([asset], `视频拼接 · ${ordered.length} 段`, meta);
  useLibraryStore.getState().addAssets([asset], `视频拼接 · ${ordered.length} 段`, meta);
  logEvent("info", "generation.video.manual_concat", { assetId: asset.id, sourceAssetIds, outputBytes: output.size });
  return asset;
}
