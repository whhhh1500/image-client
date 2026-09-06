import { convertFileSrc } from "@tauri-apps/api/core";
import { cachePromptlibImage, importRefImage, listCachedPromptlibImages } from "./ipc";
import { logEvent } from "./logger";
import { caseImageUrl, type PromptlibEntry } from "./promptlib";

const memoryCache = new Map<string, string>();
const inflight = new Map<string, Promise<string>>();
const MAX_DOWNLOADS = 6;
let activeDownloads = 0;
const waiters: Array<() => void> = [];
let hydratePromise: Promise<void> | null = null;

function fileNameOf(image: string): string {
  return image.split(/[/\\]/).filter(Boolean).pop() ?? "";
}

function acquireDownload(): Promise<void> {
  if (activeDownloads < MAX_DOWNLOADS) {
    activeDownloads += 1;
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    waiters.push(() => {
      activeDownloads += 1;
      resolve();
    });
  });
}

function releaseDownload() {
  activeDownloads = Math.max(0, activeDownloads - 1);
  const next = waiters.shift();
  if (next) next();
}

export async function hydrateCaseImageCache(): Promise<void> {
  if (!hydratePromise) {
    hydratePromise = listCachedPromptlibImages()
      .then((paths) => {
        for (const path of paths) {
          const name = fileNameOf(path);
          if (name && !memoryCache.has(name)) memoryCache.set(name, path);
        }
      })
      .catch((error) => {
        hydratePromise = null;
        logEvent("warn", "promptlib.cache_hydrate_failed", { error: String(error) });
      });
  }
  await hydratePromise;
}

/** Local disk cache first; download from R2 only on miss. */
export async function cachedCaseImagePath(image: string): Promise<string> {
  const fileName = fileNameOf(image);
  if (!fileName) throw new Error("案例图文件名无效");
  await hydrateCaseImageCache();
  const hit = memoryCache.get(fileName);
  if (hit) return hit;
  const pending = inflight.get(fileName);
  if (pending) return pending;
  const request = (async () => {
    await acquireDownload();
    try {
      const path = await cachePromptlibImage(fileName);
      memoryCache.set(fileName, path);
      return path;
    } finally {
      releaseDownload();
    }
  })().finally(() => inflight.delete(fileName));
  inflight.set(fileName, request);
  return request;
}

export async function cachedCaseImageSrc(image: string): Promise<string> {
  try {
    return convertFileSrc(await cachedCaseImagePath(image));
  } catch (error) {
    logEvent("warn", "promptlib.case_image_cache_failed", { image, error: String(error) });
    return caseImageUrl(image);
  }
}

/** Copy the cached original into the local reference-image folder. */
export async function importCaseImage(entry: PromptlibEntry): Promise<string> {
  const started = performance.now();
  logEvent("info", "promptlib.case_image.start", { id: entry.id, image: entry.image });
  const cachedPath = await cachedCaseImagePath(entry.image);
  const copied = await importRefImage(cachedPath);
  logEvent("info", "promptlib.case_image.end", {
    id: entry.id,
    status: "success",
    durationMs: performance.now() - started,
    cached: true,
  });
  return copied;
}
