import { persistAssets } from "./dbWrite";
import { compressImageFile, convertImageFile, inspectImageFile, type CompressLevel, type ConvertFormat, type ImageFileInfo } from "./ipc";
import { logEvent } from "./logger";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";

const infoCache = new Map<string, Promise<ImageFileInfo>>();
const listeners = new Set<(path: string) => void>();

export function subscribeImageInfo(listener: (path: string) => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function notifyImageInfo(path: string) {
  for (const listener of listeners) listener(path);
}

export function inspectImageCached(path: string): Promise<ImageFileInfo> {
  const hit = infoCache.get(path);
  if (hit) return hit;
  const request = inspectImageFile(path).catch((error) => {
    infoCache.delete(path);
    throw error;
  });
  infoCache.set(path, request);
  return request;
}

export function invalidateImageInfo(path: string) {
  infoCache.delete(path);
}

export function refreshImageInfo(path: string): Promise<ImageFileInfo> {
  invalidateImageInfo(path);
  const request = inspectImageCached(path);
  void request.then(() => notifyImageInfo(path)).catch(() => notifyImageInfo(path));
  return request;
}

export function latestDisplayPath(info: ImageFileInfo): string {
  return info.displayPath || info.path;
}

export async function compressHistoryImage(asset: LibAsset, level: CompressLevel): Promise<LibAsset> {
  const started = performance.now();
  logEvent("info", "image.compress.start", { path: asset.asset.path, level });
  const result = await compressImageFile(asset.asset.path, level);
  await refreshImageInfo(asset.asset.path);
  const meta = {
    model: asset.model,
    projectId: asset.projectId,
    params: {
      ...(asset.params ?? {}),
      compressedFrom: asset.asset.path,
      compressLevel: level,
    },
  };
  const source = level === "lossless" ? "无损压缩" : level === "q80" ? "有损压缩80%" : "有损压缩60%";
  await persistAssets([result], source, meta);
  useLibraryStore.getState().addAssets([result], source, meta);
  logEvent("info", "image.compress.end", {
    path: result.path,
    level,
    durationMs: performance.now() - started,
  });
  const created = useLibraryStore.getState().assets.find((item) => item.asset.id === result.id);
  return created ?? { asset: result, source, model: asset.model, projectId: asset.projectId, params: meta.params, createdAt: Date.now() };
}

export async function convertHistoryImage(asset: LibAsset, format: ConvertFormat): Promise<LibAsset> {
  const started = performance.now();
  logEvent("info", "image.convert.start", { path: asset.asset.path, format });
  const result = await convertImageFile(asset.asset.path, format);
  await refreshImageInfo(asset.asset.path);
  const meta = {
    model: asset.model,
    projectId: asset.projectId,
    params: {
      ...(asset.params ?? {}),
      convertedFrom: asset.asset.path,
      convertFormat: format,
    },
  };
  const source = `转换为 ${format.toUpperCase()}`;
  await persistAssets([result], source, meta);
  useLibraryStore.getState().addAssets([result], source, meta);
  logEvent("info", "image.convert.end", {
    path: result.path,
    format,
    durationMs: performance.now() - started,
  });
  const created = useLibraryStore.getState().assets.find((item) => item.asset.id === result.id);
  return created ?? { asset: result, source, model: asset.model, projectId: asset.projectId, params: meta.params, createdAt: Date.now() };
}

export function isCompressedAsset(asset: LibAsset): boolean {
  return Boolean(asset.params?.compressLevel)
    || Boolean(asset.params?.convertFormat)
    || /-(lossless|q80|q60|jpg|png|webp|bmp|gif)-\d{14}\.[A-Za-z0-9]+$/.test(asset.asset.path.replace(/\\/g, "/"));
}

export function generationParamsSummary(params?: Record<string, unknown>): string {
  if (!params) return "无生成参数";
  const nested = params.params && typeof params.params === "object" ? params.params as Record<string, unknown> : {};
  const pick = (key: string) => {
    const value = params[key] ?? nested[key];
    return typeof value === "string" || typeof value === "number" ? String(value) : "";
  };
  const bits = [
    pick("model") && `模型 ${pick("model")}`,
    pick("size") && `尺寸 ${pick("size")}`,
    pick("quality") && `质量 ${pick("quality")}`,
    pick("background") && `背景 ${pick("background")}`,
    Number(pick("count") || pick("n")) > 1 && `${pick("count") || pick("n")} 张`,
  ].filter(Boolean);
  return bits.join(" · ") || "无生成参数";
}
