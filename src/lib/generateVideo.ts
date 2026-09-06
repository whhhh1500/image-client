import { convertFileSrc } from "@tauri-apps/api/core";
import { cleanupVideoSegments, runVideo, saveMediaAsset, type RunNodeRequest } from "./ipc";
import { concatMp4, muxMusic } from "./media";
import { useLibraryStore } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import type { VideoParams } from "../store/useVideoStore";
import { persistAssets, persistTask } from "./dbWrite";
import { logEvent } from "./logger";
import { createId } from "./id";
import { createHistoryProvenance, snapshotAsset, type HistoryProvenance, type SourceMaterialSnapshot } from "./provenance";

export async function generateVideo(
  params: VideoParams,
  provenanceInput: Partial<Omit<HistoryProvenance, "schemaVersion" | "recordedAt">> = {},
): Promise<void> {
  const store = useLibraryStore.getState();
  const projectId = useProjectStore.getState().activeId ?? undefined;
  const taskId = createId("task");
  const nodeId = "gen_video";
  const createdAt = Date.now();
  const reference = params.referencePath.trim();
  const label = reference ? "图生视频" : "文生视频";
  const referenceAsset = reference
    ? store.assets.find((item) => item.asset.path === reference)
    : undefined;
  const defaultMaterials: SourceMaterialSnapshot[] = [];
  if (referenceAsset) defaultMaterials.push(snapshotAsset(referenceAsset, undefined, "视频参考图"));
  else if (reference) defaultMaterials.push({ kind: "file", label: "视频参考图", path: reference });
  if (params.musicEnabled && params.musicPath) defaultMaterials.push({ kind: "file", label: "背景音乐", path: params.musicPath });
  const provenance = createHistoryProvenance({
    originalInput: provenanceInput.originalInput ?? params.prompt,
    generationInput: provenanceInput.generationInput ?? params.prompt,
    systemInstruction: provenanceInput.systemInstruction,
    contextSnapshot: provenanceInput.contextSnapshot,
    sourceMaterials: [...defaultMaterials, ...(provenanceInput.sourceMaterials ?? [])],
    parentAssetIds: [...new Set([
      ...(referenceAsset ? [referenceAsset.asset.id] : []),
      ...(provenanceInput.parentAssetIds ?? []),
    ])],
    revision: provenanceInput.revision ?? { type: "generated" },
  });
  const taskParams = { ...(params as unknown as Record<string, unknown>), provenance };
  const started = performance.now();
  logEvent("info", "generation.video.start", { taskId, model: params.model, projectId, durationS: params.duration_s, aspectRatio: params.aspectRatio, resolution: params.resolution, hasReference: !!reference, musicEnabled: params.musicEnabled, prompt: params.prompt });

  store.addTask({ id: taskId, nodeId, label, model: params.model, projectId, kind: "video", status: "running", createdAt, params: taskParams });

  const config = {
    prompt: params.prompt,
    duration_s: params.duration_s,
    model: params.model,
    aspect_ratio: params.aspectRatio,
    resolution: params.resolution,
    referencePath: reference,
  };
  const req: RunNodeRequest = { nodeType: "textToVideo", category: "video", config, inputAssets: [] };
  const meta = { model: params.model, projectId, params: taskParams };

  try {
    await persistTask({ id: taskId, nodeId, providerId: "zzone", status: "running", label, model: params.model, projectId, kind: "video", createdAt, params: taskParams });
    if (reference && !/^https?:\/\//i.test(reference)) {
      throw new Error("视频参考图必须是公网 HTTP(S) URL，本地文件无法被远程视频接口读取");
    }
    const result = await runVideo(req);
    const segments = result.assets;
    if (!segments.length) throw new Error("接口未返回视频段");

    // 拼接与混音在本地（mediabunny）完成：单段直取，多段包级无损拼接。
    const segmentReadStarted = performance.now();
    const segBlobs = await Promise.all(segments.map(async (a) => {
      const response = await fetch(convertFileSrc(a.path));
      if (!response.ok) throw new Error(`读取视频分段失败: ${response.status}`);
      return response.blob();
    }));
    logEvent("info", "performance.video_segments_loaded", { taskId, segmentCount: segments.length, durationMs: performance.now() - segmentReadStarted, totalBytes: segBlobs.reduce((sum, blob) => sum + blob.size, 0) });
    const concatStarted = performance.now();
    let finalBlob = await concatMp4(segBlobs);
    logEvent("info", "performance.video_concat", { taskId, durationMs: performance.now() - concatStarted, outputBytes: finalBlob.size });
    if (params.musicEnabled && params.musicPath) {
      const response = await fetch(convertFileSrc(params.musicPath));
      if (!response.ok) throw new Error(`读取背景音乐失败: ${response.status}`);
      const musicBlob = await response.blob();
      const musicStarted = performance.now();
      finalBlob = await muxMusic(finalBlob, musicBlob);
      logEvent("info", "performance.video_music_mux", { taskId, durationMs: performance.now() - musicStarted, musicBytes: musicBlob.size, outputBytes: finalBlob.size });
    }

    const data = new Uint8Array(await finalBlob.arrayBuffer());
    const asset = await saveMediaAsset("video", "mp4", data);

    const finishedAt = Date.now();
    await persistAssets([asset], label, meta);
    await persistTask({ id: taskId, nodeId, providerId: "zzone", status: "success", label, model: params.model, projectId, kind: "video", createdAt, finishedAt, params: taskParams });
    store.addAssets([asset], label, meta);
    store.updateTask(taskId, { status: "success", finishedAt });
    try {
      const cleanup = await cleanupVideoSegments(segments.map((segment) => segment.path));
      logEvent(cleanup.failed ? "warn" : "info", "generation.video.segment_cleanup", { taskId, ...cleanup });
    } catch (error) {
      logEvent("warn", "generation.video.segment_cleanup", { taskId, status: "error", error: String(error) });
    }
    logEvent("info", "generation.video.end", { taskId, status: "success", durationMs: performance.now() - started, assetId: asset.id, outputBytes: finalBlob.size, segmentCount: segments.length });
  } catch (e) {
    const msg = String(e);
    const finishedAt = Date.now();
    store.updateTask(taskId, { status: "error", finishedAt, error: msg });
    await persistTask({ id: taskId, nodeId, providerId: "zzone", status: "error", label, model: params.model, projectId, kind: "video", createdAt, finishedAt, error: msg, params: taskParams })
      .catch((persistError) => logEvent("error", "generation.video.error_persist_failed", { taskId, error: String(persistError) }));
    logEvent("error", "generation.video.end", { taskId, status: "error", durationMs: performance.now() - started, error: msg });
    throw e;
  }
}
