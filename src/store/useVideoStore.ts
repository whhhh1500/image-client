import { create } from "zustand";

export interface VideoGenerationItem {
  id: string;
  shotNo: number;
  prompt: string;
  durationS: number;
  anchorIds?: string[];
  continuityFrom?: number | null;
  referenceStrategy?: "text" | "first_frame" | "reference";
  referenceAssetIds?: string[];
  referenceImages?: string[];
  referenceVideos?: string[];
}

export interface VideoProductionManifest {
  storyboardAssetId: string;
  anchorAssetId?: string;
  qcAssetId?: string;
  approvedModel: string;
  approvedAspectRatio: string;
  approvedResolution: string;
  shots: Array<Pick<VideoGenerationItem, "shotNo" | "prompt" | "durationS" | "referenceStrategy" | "referenceAssetIds" | "referenceImages" | "referenceVideos">>;
  approvedAt: number;
}

export interface VideoParams {
  shots: VideoGenerationItem[];
  model: string;
  aspectRatio: string;
  resolution: string;
  mode: "text" | "first_frame" | "reference";
  images: string[];
  videos: string[];
  audios: string[];
  storyboardSourceAssetId?: string;
  productionManifest?: VideoProductionManifest;
}

const DEFAULTS: VideoParams = {
  shots: [{ id: "shot-1", shotNo: 1, prompt: "", durationS: 5 }],
  model: "grok-imagine-video",
  aspectRatio: "16:9",
  resolution: "720p",
  mode: "text",
  images: [],
  videos: [],
  audios: [],
};

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string").map((item) => item.trim()).filter(Boolean) : [];
}

function normalizeShots(value: unknown): VideoGenerationItem[] {
  if (!Array.isArray(value)) return DEFAULTS.shots;
  const shots = value.flatMap((item, index) => {
    if (!item || typeof item !== "object") return [];
    const raw = item as Record<string, unknown>;
    const prompt = typeof raw.prompt === "string" ? raw.prompt.trim() : "";
    const durationS = typeof raw.durationS === "number" && Number.isSafeInteger(raw.durationS) && raw.durationS > 0 ? raw.durationS : 5;
    const anchorIds = strings(raw.anchorIds);
    const continuityFrom = raw.continuityFrom === null ? null : typeof raw.continuityFrom === "number" && Number.isSafeInteger(raw.continuityFrom) && raw.continuityFrom > 0 ? raw.continuityFrom : undefined;
    const referenceStrategy: VideoGenerationItem["referenceStrategy"] = raw.referenceStrategy === "first_frame" || raw.referenceStrategy === "reference" ? raw.referenceStrategy : raw.referenceStrategy === "text" ? "text" : undefined;
    const referenceAssetIds = strings(raw.referenceAssetIds);
    const referenceImages = strings(raw.referenceImages);
    const referenceVideos = strings(raw.referenceVideos);
    return [{ id: typeof raw.id === "string" && raw.id ? raw.id : `shot-${index + 1}`, shotNo: index + 1, prompt, durationS, ...(anchorIds.length ? { anchorIds } : {}), ...(continuityFrom !== undefined ? { continuityFrom } : {}), ...(referenceStrategy ? { referenceStrategy } : {}), ...(referenceAssetIds.length ? { referenceAssetIds } : {}), ...(referenceImages.length ? { referenceImages } : {}), ...(referenceVideos.length ? { referenceVideos } : {}) }];
  });
  return shots.length ? shots : DEFAULTS.shots;
}

export function normalizeVideoParams(params: Partial<VideoParams>): VideoParams {
  const requestedMode = params.mode;
  return {
    ...DEFAULTS,
    ...params,
    shots: normalizeShots(params.shots),
    mode: requestedMode === "text" || requestedMode === "first_frame" || requestedMode === "reference" ? requestedMode : "text",
    images: strings(params.images),
    videos: strings(params.videos),
    audios: strings(params.audios),
    storyboardSourceAssetId: typeof params.storyboardSourceAssetId === "string" && params.storyboardSourceAssetId ? params.storyboardSourceAssetId : undefined,
    productionManifest: params.productionManifest && typeof params.productionManifest === "object" ? params.productionManifest : undefined,
  };
}

export function productionManifestMismatch(params: VideoParams): string | null {
  const manifest = params.productionManifest;
  if (!manifest) return null;
  if (params.storyboardSourceAssetId !== manifest.storyboardAssetId) return "当前分镜来源已变化";
  if (params.model !== manifest.approvedModel) return `视频模型已从 ${manifest.approvedModel} 改为 ${params.model}`;
  if (params.aspectRatio !== manifest.approvedAspectRatio) return `画幅已从 ${manifest.approvedAspectRatio} 改为 ${params.aspectRatio}`;
  if (params.resolution !== manifest.approvedResolution) return `分辨率已从 ${manifest.approvedResolution} 改为 ${params.resolution}`;
  if (params.shots.length !== manifest.shots.length) return "镜头数量已变化";
  for (let index = 0; index < params.shots.length; index += 1) {
    const shot = params.shots[index];
    const approved = manifest.shots[index];
    if (shot.shotNo !== approved.shotNo || shot.prompt !== approved.prompt || shot.durationS !== approved.durationS) return `第 ${approved.shotNo} 镜的 Prompt 或时长已变化`;
    if ((shot.referenceStrategy ?? "text") !== (approved.referenceStrategy ?? "text")) return `第 ${approved.shotNo} 镜的参考方式已变化`;
    if ((shot.referenceAssetIds ?? []).join("\n") !== (approved.referenceAssetIds ?? []).join("\n")) return `第 ${approved.shotNo} 镜的参考资产已变化`;
    if ((shot.referenceImages ?? []).join("\n") !== (approved.referenceImages ?? []).join("\n") || (shot.referenceVideos ?? []).join("\n") !== (approved.referenceVideos ?? []).join("\n")) return `第 ${approved.shotNo} 镜解析出的参考素材地址已变化`;
  }
  return null;
}

interface VideoState extends VideoParams {
  set: (params: Partial<VideoParams>) => void;
  load: (params: Partial<VideoParams>) => void;
}

export const useVideoStore = create<VideoState>((set) => ({
  ...DEFAULTS,
  set: (params) => set(params),
  load: (params) => set(normalizeVideoParams(params)),
}));
