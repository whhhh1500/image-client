import { useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Check, Clapperboard, FileInput, Loader2, Plus, Send, Trash2, Upload, X } from "lucide-react";
import { productionManifestMismatch, useVideoStore, type VideoGenerationItem, type VideoLocalImageReference, type VideoParams } from "../store/useVideoStore";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { concatVideoAssets, generateVideo } from "../lib/generateVideo";
import {
  listVideoModels,
  listVideoModelCapabilities,
  assetPublishMedia,
  mediaHostingGet,
  type MediaHostingStatus,
  type VideoGenerationMode,
  type VideoModelCapability,
} from "../lib/ipc";
import { ContextMenu, menuIcons } from "../components/ContextMenu";
import AssetImportPicker from "../components/AssetImportPicker";
import MediaHostingDialog from "../components/MediaHostingDialog";
import AssetDetailModal from "../components/AssetDetailModal";
import ModelCombobox from "../components/ModelCombobox";
import { useModelCatalog } from "../lib/useModelCatalog";
import { getDocumentMeta } from "../lib/documents";
import WorkflowGuide from "../components/WorkflowGuide";
import { logEvent } from "../lib/logger";
import { snapshotAsset } from "../lib/provenance";
import { parseStoryboardShots, storyboardShotsToGenerationItems, type StoryboardShot } from "../lib/video/storyboard";
import { isPublicHttpsUrl } from "../lib/video/referenceUrl";
import { importExternalAssets } from "../lib/externalAssetImport";
import { comicMdCatalogList } from "../lib/comic/markdownApi";
import { useGenerationImportQueue } from "../store/useGenerationImportQueue";
import {
  createImportRecord,
  importEntryAssetId,
  importEntryKind,
  importEntryLabel,
  importEntryPublishedUrl,
  importEntryPrompt,
  libraryImportEntry,
  mergeImportedPrompts,
  replaceImportedPrompt,
  type ImportEntry,
} from "../lib/assetImport";

const inputCls =
  "w-full rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-fuchsia-400 focus:ring-1 focus:ring-fuchsia-400/40";

const MODE_LABELS: Record<VideoGenerationMode, string> = {
  text: "文生视频",
  first_frame: "首帧驱动",
  reference: "多素材参考",
};

type MaterialKind = "images" | "videos";
type WorkflowPhase = "idle" | "submitting" | "processing" | "completed" | "failed";

function Field({ label, children, hint }: { label: string; children: React.ReactNode; hint?: string }) {
  return (
    <label className="block">
      <div className="mb-1 flex items-center justify-between gap-2 text-xs font-medium text-slate-300">
        <span>{label}</span>
        {hint && <span className="text-right text-[10px] font-normal text-slate-500">{hint}</span>}
      </div>
      {children}
    </label>
  );
}

function parseUrls(value: string): string[] {
  return [...new Set(value.split(/[\n,]/).map((url) => url.trim()).filter(Boolean))];
}

function fallbackCapability(model: string): VideoModelCapability {
  return {
    id: model,
    label: model,
    modes: ["text", "first_frame"],
    minDurationS: 1,
    maxDurationS: 15,
    durationOptions: Array.from({ length: 15 }, (_, index) => index + 1),
    resolutions: ["480p", "720p"],
    aspectRatios: ["16:9", "9:16", "1:1"],
    maxImages: 1,
    maxVideos: 0,
    maxAudios: 0,
    maxTotalReferences: null,
    referenceImageCount: null,
    maxReferenceDurationS: null,
    note: "暂未获取该模型的能力详情；提交前会由网关再次校验。",
  };
}

export function durationOptionsFor(capability: VideoModelCapability, mode: VideoGenerationMode): number[] {
  const maximum = mode === "reference" && capability.maxReferenceDurationS
    ? capability.maxReferenceDurationS
    : capability.maxDurationS;
  return capability.durationOptions.filter((duration) => duration <= maximum);
}

/**
 * Resolve Markdown storyboard references exactly as the reviewed handoff does,
 * with one additional UI boundary: an AssetPicker import may only use media
 * from the current project. Textual anchor/document assets deliberately remain
 * in `referenceAssetIds` for provenance, but can never become provider URLs.
 */
export function resolveImportedStoryboardShots(
  storyboard: StoryboardShot[],
  assets: LibAsset[],
  belongsToCurrentProject: (projectId?: string) => boolean,
): VideoGenerationItem[] {
  const currentProjectMedia = assets.filter((asset) => (
    belongsToCurrentProject(asset.projectId)
    && (asset.asset.kind === "image" || asset.asset.kind === "video")
  ));

  return storyboardShotsToGenerationItems(storyboard).map((item) => {
    const references = item.referenceAssetIds
      .map((id) => currentProjectMedia.find((asset) => asset.asset.id === id))
      .filter((asset): asset is LibAsset => Boolean(asset));
    // Only public HTTPS URLs can be sent as provider video references. Local
    // video files used to be kept here and then blocked the whole generation
    // with no way to fix it; drop them (and log) exactly like the reviewed
    // handoff does.
    const localVideos = references.filter((asset) => asset.asset.kind === "video" && !isPublicHttpsUrl(asset.asset.path));
    if (localVideos.length) {
      logEvent("warn", "video.storyboard_local_video_skipped", { shotNo: item.shotNo, count: localVideos.length });
    }
    return {
      ...item,
      id: `shot-${item.shotNo}`,
      referenceImages: references.filter((asset) => asset.asset.kind === "image" && isPublicHttpsUrl(asset.asset.path)).map((asset) => asset.asset.path),
      referenceLocalImages: references.filter((asset) => asset.asset.kind === "image" && !isPublicHttpsUrl(asset.asset.path)).map((asset) => ({ assetId: asset.asset.id, path: asset.asset.path, label: asset.source })),
      referenceVideos: references.filter((asset) => asset.asset.kind === "video" && isPublicHttpsUrl(asset.asset.path)).map((asset) => asset.asset.path),
    };
  });
}

export function shotReferenceSummary(shot: VideoGenerationItem): string {
  const imageCount = (shot.referenceImages?.length ?? 0) + (shot.referenceLocalImages?.length ?? 0);
  const videoCount = shot.referenceVideos?.length ?? 0;
  if (!imageCount && !videoCount) return "逐镜参考 0 项";
  return `逐镜参考 ${imageCount + videoCount} 项（图片 ${imageCount} · 视频 ${videoCount}）`;
}

function desktopCapabilitySummary(capability: VideoModelCapability): string {
  const materials = [
    capability.maxImages > 0 ? `最多 ${capability.maxImages} 张图片` : "",
    capability.maxVideos > 0 ? `最多 ${capability.maxVideos} 个视频` : "",
  ].filter(Boolean).join("、");
  const totalLimit = capability.maxTotalReferences ? `，总计最多 ${capability.maxTotalReferences} 项` : "";
  return `单镜 ${capability.minDurationS}–${capability.maxDurationS} 秒；分辨率 ${capability.resolutions.join(" / ")}；${materials || "不使用参考媒体"}${totalLimit}。`;
}

export function unresolvedShotReferenceIds(
  shot: VideoGenerationItem,
  assets: LibAsset[],
  belongsToCurrentProject: (projectId?: string) => boolean,
): string[] {
  if (!shot.referenceAssetIds?.length) return [];
  const validMediaIds = new Set(assets
    .filter((asset) => belongsToCurrentProject(asset.projectId) && (asset.asset.kind === "image" || asset.asset.kind === "video"))
    .map((asset) => asset.asset.id));
  return shot.referenceAssetIds.filter((id) => !validMediaIds.has(id));
}

function chooseSupported<T>(current: T, values: T[], fallback: T): T {
  return values.includes(current) ? current : values[0] ?? fallback;
}

function UrlListField({
  label,
  values,
  max,
  onChange,
  hint,
}: {
  label: string;
  values: string[];
  max: number;
  onChange: (values: string[]) => void;
  hint: string;
}) {
  return (
    <Field label={label} hint={`${values.length}/${max} · ${hint}`}>
      <textarea
        className={`${inputCls} min-h-20 resize-y font-mono text-xs leading-relaxed`}
        value={values.join("\n")}
        placeholder="每行一个 https:// 公网 URL"
        onChange={(event) => onChange(parseUrls(event.target.value).slice(0, max))}
        spellCheck={false}
      />
      <p className="mt-1 text-[10px] leading-snug text-slate-500">仅会提交公网 HTTPS URL；本地文件和内网地址不能传给视频服务。</p>
    </Field>
  );
}

function workflowStepClass(phase: WorkflowPhase, index: number): string {
  const activeIndex = phase === "submitting" ? 0 : phase === "processing" ? 1 : phase === "completed" ? 3 : -1;
  if (phase === "failed") return index === 0 ? "border-rose-300/40 bg-rose-300/10 text-rose-100" : "border-slate-800 text-slate-600";
  if (index <= activeIndex) return "border-fuchsia-300/35 bg-fuchsia-300/10 text-fuchsia-100";
  return "border-slate-800 bg-slate-950/20 text-slate-600";
}

/** Coalesce shot-prompt keystrokes before they touch the shared video store. */
const SHOT_PROMPT_DEBOUNCE_MS = 250;

/**
 * Local-state prompt field for one shot. Writing every keystroke to the store
 * re-rendered this whole 860-line panel (and re-ran its validators), so edits
 * are kept locally and committed after a short pause or on blur.
 */
function ShotPromptTextarea({
  value,
  onCommit,
  className,
  placeholder,
}: {
  value: string;
  onCommit: (prompt: string) => void;
  className: string;
  placeholder: string;
}) {
  const [draft, setDraft] = useState(value);
  const committedRef = useRef(value);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    // Adopt external changes (import/template/reset) but never clobber typing.
    if (value === committedRef.current) return;
    committedRef.current = value;
    setDraft(value);
  }, [value]);

  useEffect(() => () => {
    if (timerRef.current !== null) clearTimeout(timerRef.current);
  }, []);

  const commit = (next: string) => {
    if (timerRef.current !== null) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    if (next === committedRef.current) return;
    committedRef.current = next;
    onCommit(next);
  };

  return (
    <textarea
      className={className}
      value={draft}
      placeholder={placeholder}
      onChange={(event) => {
        const next = event.target.value;
        setDraft(next);
        if (timerRef.current !== null) clearTimeout(timerRef.current);
        timerRef.current = setTimeout(() => {
          timerRef.current = null;
          commit(next);
        }, SHOT_PROMPT_DEBOUNCE_MS);
      }}
      onBlur={() => commit(draft)}
    />
  );
}

export default function VideoPanel() {
  const vid = useVideoStore();
  const [capabilities, setCapabilities] = useState<VideoModelCapability[]>([]);
  const videoCatalog = useModelCatalog("video", []);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [phase, setPhase] = useState<WorkflowPhase>("idle");
  const [selectedVideoIds, setSelectedVideoIds] = useState<string[]>([]);
  const [joining, setJoining] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number; asset: LibAsset } | null>(null);
  const [playing, setPlaying] = useState<LibAsset | null>(null);
  const [guideAsset, setGuideAsset] = useState<LibAsset | null>(null);
  const [videoGuideAsset, setVideoGuideAsset] = useState<LibAsset | null>(null);
  const [textSource, setTextSource] = useState<LibAsset | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerPreset, setPickerPreset] = useState<{ entries: ImportEntry[]; action: "prompt" | "merge_prompt" | "reference" } | null>(null);
  const [importShotId, setImportShotId] = useState<string | null>(null);
  const [canonicalComicEntries, setCanonicalComicEntries] = useState<ImportEntry[]>([]);
  const [hostingDialogOpen, setHostingDialogOpen] = useState(false);
  const [hostingStatus, setHostingStatus] = useState<MediaHostingStatus | null>(null);
  const publishedMedia = useRef(new Map<string, { url: string; sha256?: string }>());
  const pickerSession = useRef(0);
  const processingTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const projects = useProjectStore((s) => s.projects);
  const activeId = useProjectStore((s) => s.activeId);
  const defaultId = projects[0]?.id;
  const belongs = (projectId?: string) => projectId === activeId || (!projectId && activeId === defaultId);
  const allAssets = useLibraryStore((state) => state.assets);
  const pendingImport = useGenerationImportQueue((state) => state.pending);
  const importEntries = useMemo(() => [
    ...allAssets.filter((asset) => belongs(asset.projectId)).map((asset) => ({ entryType: "library_asset" as const, asset })),
    ...canonicalComicEntries,
  ], [allAssets, canonicalComicEntries, activeId, defaultId]);
  // 视频工作区仅展示 video 资产；图片只可经“创作参考”显式导入，不会混入视频历史。
  const videos = allAssets.filter((item) => item.asset.kind === "video" && belongs(item.projectId));

  useEffect(() => {
    let disposed = false;
    void Promise.allSettled([listVideoModels(), listVideoModelCapabilities()])
      .then(([modelsResult, capabilitiesResult]) => {
        if (disposed) return;
        if (modelsResult.status === "fulfilled") videoCatalog.setOptions(modelsResult.value);
        if (capabilitiesResult.status === "fulfilled") setCapabilities(capabilitiesResult.value);
        const failures = [
          modelsResult.status === "rejected" ? `模型目录：${String(modelsResult.reason)}` : "",
          capabilitiesResult.status === "rejected" ? `能力定义：${String(capabilitiesResult.reason)}` : "",
        ].filter(Boolean);
        if (failures.length) setError(`读取视频模型信息失败：${failures.join("；")}`);
      });
    return () => {
      disposed = true;
      if (processingTimer.current) clearTimeout(processingTimer.current);
    };
    // 初始化只需运行一次；vid 是 zustand store 的合成对象。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const projectId = activeId ?? defaultId;
    if (!projectId) { setCanonicalComicEntries([]); return; }
    let cancelled = false;
    void comicMdCatalogList({ projectId }).then((items) => {
      if (cancelled || (useProjectStore.getState().activeId ?? useProjectStore.getState().projects[0]?.id) !== projectId) return;
      setCanonicalComicEntries(items.map((item) => ({ entryType: "canonical_comic" as const, readonly: true as const, ...item })));
    }).catch((cause) => logEvent("warn", "asset_import.comic_catalog_failed", { projectId, error: String(cause) }));
    return () => { cancelled = true; };
  }, [activeId, defaultId]);

  useEffect(() => {
    if (!pendingImport || pendingImport.target !== "video") return;
    const projectId = useProjectStore.getState().activeId ?? useProjectStore.getState().projects[0]?.id;
    if (!projectId || pendingImport.projectId !== projectId) {
      useGenerationImportQueue.getState().clear(pendingImport.requestId);
      return;
    }
    setPickerPreset({ entries: pendingImport.entries, action: pendingImport.action });
    setPickerOpen(true);
    useGenerationImportQueue.getState().clear(pendingImport.requestId);
  }, [pendingImport?.requestId]);

  useEffect(() => {
    if (!pickerOpen) return;
    let cancelled = false;
    void mediaHostingGet().then((status) => !cancelled && setHostingStatus(status)).catch((cause) => {
      if (!cancelled) logEvent("warn", "media_hosting.status_failed", { error: String(cause) });
    });
    return () => { cancelled = true; };
  }, [pickerOpen]);

  const knownCapability = useMemo(
    () => capabilities.find((item) => item.id === vid.model),
    [capabilities, vid.model],
  );
  const capability = knownCapability ?? fallbackCapability(vid.model);
  const advertisedModels = videoCatalog.options.length ? videoCatalog.options : capabilities.map((item) => item.id);
  const modelOptions = [...new Set([vid.model, ...advertisedModels])].filter(Boolean);
  const modelLabels = Object.fromEntries(capabilities.map((item) => [item.id, item.label]));
  const images = vid.images;
  const hasMaterials = images.length + vid.videos.length > 0;
  const shots = vid.shots;
  const durationOptions = durationOptionsFor(capability, vid.mode);

  const validationError = useMemo(() => {
    if (capabilities.length > 0 && !knownCapability) return `当前客户端没有 ${vid.model} 的能力定义，请选择已验证模型或更新客户端。`;
    const manifestIssue = productionManifestMismatch(vid);
    if (manifestIssue) return `已审查生产条件失效：${manifestIssue}。请回到短剧 Agent 重新审查，或解除审查清单后作为普通视频任务生成。`;
    if (vid.audios.length) return "当前视频工作区暂不处理声音或音频参考，请移除旧任务中的音频参数后再生成。";
    for (const shot of shots) {
      const shotMode = shot.referenceStrategy ?? vid.mode;
      // A text shot never sends media references, and its referenceAssetIds may
      // legitimately hold text anchor ids for provenance. storyboardProductionIssues
      // skips the same check for text shots, so blocking here contradicted it.
      const unresolvedReferences = shotMode === "text" ? [] : unresolvedShotReferenceIds(shot, allAssets, belongs);
      if (unresolvedReferences.length) return `第 ${shot.shotNo} 镜包含不可用的参考资产：${unresolvedReferences.join("、")}。参考资产必须是当前项目的图像或视频资源。`;
      const shotDurations = durationOptionsFor(capability, shotMode);
      if (!shotDurations.includes(shot.durationS)) return `${capability.label} 在 ${shotMode} 模式下不支持第 ${shot.shotNo} 镜的 ${shot.durationS} 秒时长。`;
      const localImages = shot.referenceLocalImages ?? [];
      const shotImages = (shot.referenceImages?.length || localImages.length) ? shot.referenceImages ?? [] : images;
      const shotVideos = shot.referenceVideos?.length ? shot.referenceVideos : vid.videos;
      const shotMaterials = [...shotImages, ...shotVideos];
      if (shotMaterials.some((value) => !isPublicHttpsUrl(value))) return `第 ${shot.shotNo} 镜的参考素材必须是无凭据的公网 HTTPS URL，不能使用本机或内网地址。`;
      const imageCount = shotImages.length + localImages.length;
      if (imageCount > capability.maxImages || shotVideos.length > capability.maxVideos) return `第 ${shot.shotNo} 镜的参考素材超过当前模型上限。`;
      if (capability.maxTotalReferences && imageCount + shotVideos.length > capability.maxTotalReferences) return `第 ${shot.shotNo} 镜的参考素材总数超过当前模型上限（${imageCount + shotVideos.length}/${capability.maxTotalReferences}）。`;
      if (shotMode === "text" && (shot.referenceImages?.length || shot.referenceVideos?.length || localImages.length)) return `第 ${shot.shotNo} 镜为文生模式，不能携带逐镜参考素材。`;
      if (shotMode === "first_frame" && (imageCount !== 1 || shotVideos.length)) return `第 ${shot.shotNo} 镜的首帧模式需要且只能提供 1 张图片。`;
      if (shotMode === "reference" && !imageCount && !shotVideos.length) return `第 ${shot.shotNo} 镜的多素材参考模式缺少参考素材。`;
      if (shotMode === "reference" && capability.referenceImageCount && imageCount !== capability.referenceImageCount) return `第 ${shot.shotNo} 镜必须恰好提供 ${capability.referenceImageCount} 张参考图片。`;
    }
    const materialGroups: Array<[string, string[], number]> = [
      ["图片", images, capability.maxImages],
      ["视频", vid.videos, capability.maxVideos],
    ];
    for (const [label, values, max] of materialGroups) {
      if (values.length > max) return `${label}素材超过当前模型上限（${values.length}/${max}）。`;
      if (values.some((value) => !isPublicHttpsUrl(value))) return `${label}素材必须是无凭据的公网 HTTPS URL，不能使用本机或内网地址。`;
    }
    if (capability.maxTotalReferences && images.length + vid.videos.length > capability.maxTotalReferences) return `参考素材总数超过当前模型上限（${images.length + vid.videos.length}/${capability.maxTotalReferences}）。`;
    if (!capability.resolutions.includes(vid.resolution)) return `当前模型不支持 ${vid.resolution}。`;
    if (!capability.aspectRatios.includes(vid.aspectRatio)) return `当前模型不支持 ${vid.aspectRatio} 画幅。`;
    if (vid.mode === "text" && hasMaterials) return "文生模式不能携带参考素材；请切换为首帧或多素材参考模式。";
    const hasExplicitShotReferences = shots.some((shot) => (shot.referenceImages?.length ?? 0) + (shot.referenceVideos?.length ?? 0) + (shot.referenceLocalImages?.length ?? 0) > 0);
    if (vid.mode === "first_frame" && !hasExplicitShotReferences && images.length !== 1) return "首帧驱动模式需要且只能提供 1 张图片 URL。";
    if (vid.mode === "reference" && !hasMaterials && !shots.some((shot) => (shot.referenceImages?.length ?? 0) + (shot.referenceVideos?.length ?? 0) + (shot.referenceLocalImages?.length ?? 0) > 0)) return "多素材参考模式至少需要一条图片或视频参考。";
    if (vid.mode === "reference" && !hasExplicitShotReferences && capability.referenceImageCount && images.length !== capability.referenceImageCount) {
      return `当前模型的多素材参考模式必须恰好提供 ${capability.referenceImageCount} 张图片。`;
    }
    if (vid.videos.length && vid.model === "drama-video-v2" && !images.length) {
      return "Drama Video 使用视频参考时，必须同时提供至少一张图片。";
    }
    if (!shots.length || shots.some((shot) => !shot.prompt.trim())) return "每个视频镜头都需要填写 Prompt。";
    return null;
  }, [activeId, allAssets, capabilities.length, capability, defaultId, hasMaterials, images, knownCapability, shots, vid.aspectRatio, vid.audios, vid.mode, vid.model, vid.productionManifest, vid.resolution, vid.storyboardSourceAssetId, vid.videos]);

  const selectModel = (model: string) => {
    const next = capabilities.find((item) => item.id === model) ?? fallbackCapability(model);
    const mode = chooseSupported(vid.mode, next.modes, "text");
    vid.set({
      model,
      mode,
      shots: shots.map((shot) => {
        const supportedDurations = durationOptionsFor(next, shot.referenceStrategy ?? mode);
        return { ...shot, durationS: chooseSupported(shot.durationS, supportedDurations, supportedDurations[0] ?? next.minDurationS) };
      }),
      resolution: chooseSupported(vid.resolution, next.resolutions, "720p"),
      aspectRatio: chooseSupported(vid.aspectRatio, next.aspectRatios, "16:9"),
    });
    setError(null);
  };

  const selectMode = (mode: VideoGenerationMode) => {
    vid.set({
      mode,
      shots: shots.map((shot) => {
        const supportedDurations = durationOptionsFor(capability, shot.referenceStrategy ?? mode);
        return { ...shot, durationS: chooseSupported(shot.durationS, supportedDurations, supportedDurations[0] ?? capability.minDurationS) };
      }),
      ...(mode === "text" ? { images: [], videos: [], audios: [] } : {}),
      ...(mode === "first_frame" ? { images: images.slice(0, 1), videos: [], audios: [] } : {}),
    });
    setError(null);
  };

  const setMaterials = (kind: MaterialKind, values: string[]) => {
    const clean = values.map((item) => item.trim()).filter(Boolean);
    if (kind === "images") vid.set({ images: clean });
    else vid.set({ videos: clean });
  };

  const updateShots = (items: VideoGenerationItem[]) => {
    const next = items.length ? items : [{ id: "shot-1", shotNo: 1, prompt: "", durationS: durationOptions[0] ?? capability.minDurationS }];
    vid.set({ shots: next.map((shot, index) => ({ ...shot, shotNo: index + 1 })) });
    setTextSource(null);
  };

  const commitShotPrompt = (shotId: string, prompt: string) => {
    // Read the live shots: a debounced commit may land after other edits.
    updateShots(useVideoStore.getState().shots.map((item) => (item.id === shotId ? { ...item, prompt } : item)));
  };

  const importTarget = shots.find((shot) => shot.id === importShotId) ?? shots[0];
  const applyImportedVideoAssets = async ({ action, referenceMode, localImageDelivery = "direct", entries }: { action: "prompt" | "merge_prompt" | "reference"; referenceMode: "replace" | "append"; localImageDelivery?: "direct" | "hosting"; entries: ImportEntry[] }) => {
    const sessionAtStart = pickerSession.current;
    if (vid.productionManifest) throw new Error("已加载审查生产清单；请先明确解除审查清单，才可导入普通视频素材。");
    if (!importTarget) throw new Error("没有可接收导入内容的视频镜头。");
    if (action !== "reference") {
      const usable = entries.filter((entry) => Boolean(importEntryPrompt(entry)));
      if (!usable.length) throw new Error("所选内容没有可见正文或已保存提示词，不能形成视频 Prompt。");
      const excluded = entries.filter((entry) => !importEntryPrompt(entry));
      const prompt = action === "prompt" ? replaceImportedPrompt(usable) : mergeImportedPrompts(importTarget.prompt, usable);
      if (!prompt) throw new Error("所选内容不能形成视频 Prompt。");
      const singleLibrarySource = action === "prompt" && usable.length === 1 && usable[0].entryType === "library_asset" ? usable[0].asset : undefined;
      const storyboard = singleLibrarySource ? parseStoryboardShots(prompt) : [];
      if (storyboard.length) {
        updateShots(resolveImportedStoryboardShots(storyboard, allAssets, belongs));
        const record = { ...createImportRecord("prompt", usable, "导入视频分镜"), targetShotId: importTarget.id };
        vid.set({ storyboardSourceAssetId: singleLibrarySource!.asset.id, importedSources: [...(vid.importedSources ?? []).filter((item) => item.targetShotId !== importTarget.id), record] });
        return;
      }
      updateShots(shots.map((shot) => shot.id === importTarget.id ? { ...shot, prompt } : shot));
      const record = { ...createImportRecord(action, usable, action === "prompt" ? "导入视频提示词" : "合并视频提示词"), targetShotId: importTarget.id };
      vid.set({ importedSources: action === "prompt"
        ? [...(vid.importedSources ?? []).filter((item) => item.targetShotId !== importTarget.id || item.action === "reference"), record]
        : [...(vid.importedSources ?? []), record] });
      if (excluded.length) setError(`已排除 ${excluded.length} 项没有可见正文/已保存提示词的资源：${excluded.map((entry) => entry.entryType === "library_asset" ? entry.asset.source : entry.title).join("、")}`);
      return;
    }

    const media = entries.map((entry) => ({
      entry,
      kind: importEntryKind(entry),
      path: entry.entryType === "library_asset" ? entry.asset.asset.path : entry.path,
      publishedUrl: importEntryPublishedUrl(entry),
      source: entry.entryType === "library_asset" ? { assetId: entry.asset.asset.id } : { sourceUri: entry.sourceUri },
    }));
    if (media.some((item) => item.kind === "text" || (!item.path && !item.publishedUrl))) throw new Error("所选参考中包含没有媒体文件的资料，未应用选择。");
    const selectedImages = media.filter((item) => item.kind === "image");
    const selectedVideos = media.filter((item) => item.kind === "video");
    const existingImages = referenceMode === "append" ? importTarget.referenceImages ?? [] : [];
    const existingVideos = referenceMode === "append" ? importTarget.referenceVideos ?? [] : [];
    const existingLocalImages = referenceMode === "append" ? importTarget.referenceLocalImages ?? [] : [];
    const projectId = activeId ?? defaultId;
    if (!projectId) throw new Error("请先选择项目，再上传本地参考媒体。");
    const projectIdForEntry = (entry: ImportEntry) => entry.entryType === "library_asset" ? entry.asset.projectId : entry.projectId;
    const belongsToCurrentProject = (entry: ImportEntry) => {
      const entryPid = projectIdForEntry(entry);
      return entryPid === projectId || (!entryPid && projectId === defaultId);
    };
    if (media.some((item) => !belongsToCurrentProject(item.entry))) throw new Error("项目已切换，旧项目的导入选择已清除。请在当前项目重新选择。");
    const sourceKey = (entry: ImportEntry) => entry.entryType === "library_asset" ? `asset:${entry.asset.asset.id}` : `comic:${entry.sourceUri}`;
    const cacheKey = (endpoint: string, entry: ImportEntry) => `${endpoint}:${projectIdForEntry(entry) || projectId}:${sourceKey(entry)}`;
    let cacheEndpoint = hostingStatus?.endpoint ?? "";
    const cachedUrl = (item: typeof media[number]) => publishedMedia.current.get(cacheKey(cacheEndpoint, item.entry))?.url;
    const referenceUrl = (item: typeof media[number]) => item.publishedUrl ?? (item.path && isPublicHttpsUrl(item.path) ? item.path : undefined) ?? cachedUrl(item);
    const localImageReference = (item: typeof media[number]): VideoLocalImageReference => ({
      ...(item.entry.entryType === "library_asset" ? { assetId: item.entry.asset.asset.id } : { sourceUri: item.entry.sourceUri }),
      path: item.path!,
      label: importEntryLabel(item.entry),
    });
    const isLocalImage = (item: typeof media[number]) => item.kind === "image" && !item.publishedUrl && !(item.path && isPublicHttpsUrl(item.path));
    const selectedDirectLocalImages = localImageDelivery === "direct"
      ? selectedImages.filter(isLocalImage).map(localImageReference)
      : [];
    const selectedDirectKeys = new Set(selectedDirectLocalImages.map((item) => item.assetId ? `asset:${item.assetId}` : `comic:${item.sourceUri}`));
    const selectedUrlImages = selectedImages.filter((item) => !selectedDirectKeys.has(sourceKey(item.entry)));
    const candidateImageCount = new Set([...existingImages, ...selectedUrlImages.map((item) => referenceUrl(item) ?? `host:${sourceKey(item.entry)}`), ...existingLocalImages.map((item) => `local:${item.assetId ?? item.sourceUri}`), ...selectedDirectLocalImages.map((item) => `local:${item.assetId ?? item.sourceUri}`)]).size;
    const candidateVideoCount = new Set([...existingVideos, ...selectedVideos.map((item) => referenceUrl(item) ?? `local:${item.entry.entryType === "library_asset" ? item.entry.asset.asset.id : item.entry.sourceUri}`)]).size;
    if (candidateImageCount > capability.maxImages || candidateVideoCount > capability.maxVideos) throw new Error(`${capability.label} 的参考槽位不足：图片 ${candidateImageCount}/${capability.maxImages}，视频 ${candidateVideoCount}/${capability.maxVideos}。未应用选择。`);
    if (capability.maxTotalReferences && candidateImageCount + candidateVideoCount > capability.maxTotalReferences) throw new Error(`${capability.label} 的参考素材总槽位为 ${capability.maxTotalReferences}，未应用选择。`);
    const desiredStrategy = candidateImageCount === 1 && candidateVideoCount === 0 && capability.modes.includes("first_frame")
      ? "first_frame" as const
      : "reference" as const;
    if (!capability.modes.includes(desiredStrategy)) throw new Error(`${capability.label} 不支持当前参考素材组合。`);
    if (desiredStrategy === "reference" && capability.referenceImageCount && candidateImageCount !== capability.referenceImageCount) throw new Error(`${capability.label} 的多素材参考必须恰好有 ${capability.referenceImageCount} 张图片，未应用选择。`);
    if (vid.model === "drama-video-v2" && candidateVideoCount && !candidateImageCount) throw new Error("Drama Video 使用视频参考时必须同时选择至少一张公网图片，未应用选择。");
    const shotFingerprint = (shot: VideoGenerationItem | undefined) => shot ? JSON.stringify({
      id: shot.id,
      prompt: shot.prompt,
      durationS: shot.durationS,
      referenceStrategy: shot.referenceStrategy ?? null,
      referenceAssetIds: shot.referenceAssetIds ?? [],
      referenceImages: shot.referenceImages ?? [],
      referenceVideos: shot.referenceVideos ?? [],
      referenceLocalImages: shot.referenceLocalImages ?? [],
    }) : null;
    const targetFingerprint = shotFingerprint(importTarget);
    const manifestFingerprint = JSON.stringify(vid.productionManifest ?? null);
    const requestFingerprint = JSON.stringify({ model: vid.model, mode: vid.mode, resolution: vid.resolution, aspectRatio: vid.aspectRatio });
    const importStillCurrent = () => {
      const current = useVideoStore.getState();
      const currentProjectId = useProjectStore.getState().activeId ?? useProjectStore.getState().projects[0]?.id;
      return currentProjectId === projectId
        && pickerSession.current === sessionAtStart
        && JSON.stringify(current.productionManifest ?? null) === manifestFingerprint
        && JSON.stringify({ model: current.model, mode: current.mode, resolution: current.resolution, aspectRatio: current.aspectRatio }) === requestFingerprint
        && shotFingerprint(current.shots.find((shot) => shot.id === importTarget.id)) === targetFingerprint;
    };
    const unpublished = [...selectedUrlImages, ...selectedVideos].filter((item) => !referenceUrl(item));
    if (unpublished.length) {
      const hosting = await mediaHostingGet();
      if (!hosting.configured) { setHostingDialogOpen(true); throw new Error("请先配置媒体托管服务，再上传并引用本地媒体。"); }
      if (hostingStatus && hosting.endpoint !== hostingStatus.endpoint) {
        setHostingStatus(hosting);
        throw new Error("媒体托管服务已变更，已刷新发送目标。请确认后再次点击“上传并引用”。");
      }
      cacheEndpoint = hosting.endpoint;
      setHostingStatus(hosting);
      if (!importStillCurrent()) throw new Error("当前项目、审查清单或目标镜头已变化，未开始上传。请重新选择后应用。");
      const published = await assetPublishMedia({ projectId, expectedEndpoint: hosting.endpoint, sources: unpublished.map((item) => item.source) });
      const failed = published.results.filter((item) => item.error || !item.url);
      for (const result of published.results) {
        if (!result.url) continue;
        const key = `${hosting.endpoint}:${projectId}:${result.key}`;
        publishedMedia.current.set(key, { url: result.url, sha256: result.sha256 });
      }
      if (!importStillCurrent()) {
        throw new Error("上传已完成，但当前项目、审查清单或目标镜头已变化，结果没有回填。请重新选择后应用。");
      }
      const labels = new Map(media.map((item) => [sourceKey(item.entry), importEntryLabel(item.entry)]));
      if (failed.length) throw new Error(`已上传 ${published.results.length - failed.length} 项；${failed.map((item) => `${labels.get(item.key) ?? "参考媒体"}: ${item.error ?? "未返回 URL"}`).join("；")}。再次点击“上传并引用”只会重试失败项。`);
    }
    const resolvedImages = [...new Set([...existingImages, ...selectedUrlImages.map(referenceUrl).filter((value): value is string => Boolean(value))])];
    const resolvedVideos = [...new Set([...existingVideos, ...selectedVideos.map(referenceUrl).filter((value): value is string => Boolean(value))])];
    const resolvedLocalImages = [...existingLocalImages, ...selectedDirectLocalImages].filter((item, index, values) => values.findIndex((candidate) => candidate.assetId === item.assetId && candidate.sourceUri === item.sourceUri) === index);
    if (resolvedImages.length + resolvedLocalImages.length !== candidateImageCount || resolvedVideos.length !== candidateVideoCount) throw new Error("媒体托管未返回有效 HTTPS 地址，未应用选择。");
    if ([...resolvedImages, ...resolvedVideos].some((value) => !isPublicHttpsUrl(value))) throw new Error("媒体托管返回的参考地址不是无凭据的公网 HTTPS URL，未应用选择。");
    const selectedIds = media.flatMap((item) => importEntryAssetId(item.entry) ?? []);
    const hostedKeys = new Set([...selectedUrlImages, ...selectedVideos].map((item) => sourceKey(item.entry)));
    const hostedEntries = entries.map((entry) => {
      if (!hostedKeys.has(sourceKey(entry))) return entry;
      const key = cacheKey(cacheEndpoint, entry);
      const cached = publishedMedia.current.get(key);
      return cached ? { ...entry, publishedUrl: cached.url, sha256: cached.sha256 } as ImportEntry : entry;
    });
    const record = { ...createImportRecord("reference", hostedEntries, "视频实际模型参考"), targetShotId: importTarget.id };
    const currentState = useVideoStore.getState();
    currentState.set({ shots: currentState.shots.map((shot) => shot.id !== importTarget.id ? shot : {
      ...shot,
      referenceStrategy: desiredStrategy,
      referenceImages: resolvedImages,
      referenceVideos: resolvedVideos,
      referenceLocalImages: resolvedLocalImages,
      referenceAssetIds: [...new Set([...(referenceMode === "append" ? shot.referenceAssetIds ?? [] : []), ...selectedIds])],
    }),
      mode: desiredStrategy,
      importedSources: [...(referenceMode === "append" ? currentState.importedSources ?? [] : (currentState.importedSources ?? []).filter((item) => item.targetShotId !== importTarget.id || item.action !== "reference")), record],
    });
    setError(null);
  };

  const importExternalMedia = async () => {
    const projectId = activeId ?? defaultId;
    if (!projectId) throw new Error("请先选择项目，再将外部素材导入资产库。");
    const chosen = await openDialog({ multiple: true, filters: [{ name: "图片或视频", extensions: ["png", "jpg", "jpeg", "webp", "mp4", "mov", "webm"] }] });
    const paths = Array.isArray(chosen) ? chosen : chosen ? [chosen] : [];
    if (!paths.length) return;
    const imported = await importExternalAssets({
      projectId,
      importEntry: "video_reference",
      files: paths.map((path) => ({ path })),
      params: { referenceRole: "video_generation" },
    });
    if ((useProjectStore.getState().activeId ?? useProjectStore.getState().projects[0]?.id) !== projectId) return;
    setPickerPreset({ entries: imported.map(libraryImportEntry), action: "reference" });
    setPickerOpen(true);
    setNotice(`已将 ${imported.length} 个外部素材保存到资产库；请确认它们作为本地图片或托管 URL 参考的发送方式。`);
  };

  const removeLocalImageReference = (shotId: string, local: NonNullable<VideoGenerationItem["referenceLocalImages"]>[number]) => {
    const current = useVideoStore.getState();
    const sameLocal = (material: { assetId?: string; source?: string; path?: string }) => {
      // Imported identities are authoritative.  Falling back to a path is
      // only for legacy snapshots with neither identity, because two catalog
      // assets are allowed to point at the same local file.
      if (local.assetId) return material.assetId === local.assetId;
      if (local.sourceUri) return material.source === local.sourceUri;
      return !material.assetId && !material.source && material.path === local.path;
    };
    const importedSources = (current.importedSources ?? []).flatMap((record) => {
      if (record.targetShotId !== shotId || record.action !== "reference") return [record];
      const sourceMaterials = record.sourceMaterials.filter((material) => !sameLocal(material) || Boolean(material.publishedUrl));
      return sourceMaterials.length ? [{
        ...record,
        sourceMaterials,
        // A reference record's ids mirror its remaining material.  This keeps
        // the hosted half of an image when its direct-local half is removed.
        assetIds: [...new Set(sourceMaterials.flatMap((material) => material.assetId ? [material.assetId] : []))],
      }] : [];
    });
    const target = current.shots.find((shot) => shot.id === shotId);
    const remainingLocalAssetIds = new Set((target?.referenceLocalImages ?? [])
      .filter((item) => item.assetId !== local.assetId || item.sourceUri !== local.sourceUri)
      .flatMap((item) => item.assetId ? [item.assetId] : []));
    const retainedAssetIds = new Set([...remainingLocalAssetIds, ...importedSources
      .filter((record) => record.targetShotId === shotId && record.action === "reference")
      .flatMap((record) => record.sourceMaterials.flatMap((material) => material.assetId ? [material.assetId] : []))]);
    current.set({
      shots: current.shots.map((shot) => {
        if (shot.id !== shotId) return shot;
        const nextLocal = (shot.referenceLocalImages ?? []).filter((item) => item.assetId !== local.assetId || item.sourceUri !== local.sourceUri);
        const remainingReferences = nextLocal.length
          + (shot.referenceImages?.length ?? 0)
          + (shot.referenceVideos?.length ?? 0);
        return {
          ...shot,
          referenceLocalImages: nextLocal,
          referenceAssetIds: local.assetId && !retainedAssetIds.has(local.assetId)
            ? (shot.referenceAssetIds ?? []).filter((id) => id !== local.assetId)
            : shot.referenceAssetIds,
          referenceStrategy: remainingReferences ? shot.referenceStrategy : undefined,
        };
      }),
      importedSources,
    });
  };

  const removeHostedImageReference = (shotId: string, url: string) => {
    const current = useVideoStore.getState();
    const removedAssetIds = new Set((current.importedSources ?? []).filter((record) => record.targetShotId === shotId && record.action === "reference").flatMap((record) => record.sourceMaterials.filter((material) => material.publishedUrl === url || material.path === url).flatMap((material) => material.assetId ? [material.assetId] : [])));
    const importedSources = (current.importedSources ?? []).flatMap((record) => {
      if (record.targetShotId !== shotId || record.action !== "reference") return [record];
      const sourceMaterials = record.sourceMaterials.filter((material) => material.publishedUrl !== url && material.path !== url);
      return sourceMaterials.length ? [{
        ...record,
        sourceMaterials,
        assetIds: [...new Set(sourceMaterials.flatMap((material) => material.assetId ? [material.assetId] : []))],
      }] : [];
    });
    const target = current.shots.find((shot) => shot.id === shotId);
    const localAssetIds = new Set((target?.referenceLocalImages ?? []).flatMap((item) => item.assetId ? [item.assetId] : []));
    const retainedAssetIds = new Set([...localAssetIds, ...importedSources
      .filter((record) => record.targetShotId === shotId && record.action === "reference")
      .flatMap((record) => record.sourceMaterials.flatMap((material) => material.assetId ? [material.assetId] : []))]);
    current.set({
      shots: current.shots.map((shot) => {
        if (shot.id !== shotId) return shot;
        const nextImages = (shot.referenceImages ?? []).filter((item) => item !== url);
        const remainingReferences = nextImages.length
          + (shot.referenceLocalImages?.length ?? 0)
          + (shot.referenceVideos?.length ?? 0);
        return {
          ...shot,
          referenceImages: nextImages,
          referenceAssetIds: (shot.referenceAssetIds ?? []).filter((id) => !removedAssetIds.has(id) || retainedAssetIds.has(id)),
          referenceStrategy: remainingReferences ? shot.referenceStrategy : undefined,
        };
      }),
      importedSources,
    });
  };

  const removeHostedVideoReference = (shotId: string, url: string) => {
    const current = useVideoStore.getState();
    const removedAssetIds = new Set((current.importedSources ?? []).filter((record) => record.targetShotId === shotId && record.action === "reference").flatMap((record) => record.sourceMaterials.filter((material) => material.publishedUrl === url || material.path === url).flatMap((material) => material.assetId ? [material.assetId] : [])));
    const importedSources = (current.importedSources ?? []).flatMap((record) => {
      if (record.targetShotId !== shotId || record.action !== "reference") return [record];
      const sourceMaterials = record.sourceMaterials.filter((material) => material.publishedUrl !== url && material.path !== url);
      return sourceMaterials.length ? [{
        ...record,
        sourceMaterials,
        assetIds: [...new Set(sourceMaterials.flatMap((material) => material.assetId ? [material.assetId] : []))],
      }] : [];
    });
    const target = current.shots.find((shot) => shot.id === shotId);
    const localAssetIds = new Set((target?.referenceLocalImages ?? []).flatMap((item) => item.assetId ? [item.assetId] : []));
    const retainedAssetIds = new Set([...localAssetIds, ...importedSources
      .filter((record) => record.targetShotId === shotId && record.action === "reference")
      .flatMap((record) => record.sourceMaterials.flatMap((material) => material.assetId ? [material.assetId] : []))]);
    current.set({
      shots: current.shots.map((shot) => {
        if (shot.id !== shotId) return shot;
        const nextVideos = (shot.referenceVideos ?? []).filter((item) => item !== url);
        const remainingReferences = (shot.referenceImages?.length ?? 0)
          + (shot.referenceLocalImages?.length ?? 0)
          + nextVideos.length;
        return {
          ...shot,
          referenceVideos: nextVideos,
          referenceAssetIds: (shot.referenceAssetIds ?? []).filter((id) => !removedAssetIds.has(id) || retainedAssetIds.has(id)),
          referenceStrategy: remainingReferences ? shot.referenceStrategy : undefined,
        };
      }),
      importedSources,
    });
  };

  const run = async () => {
    if (validationError || busy) return;
    setBusy(true);
    setError(null);
    setPhase("submitting");
    processingTimer.current = setTimeout(() => setPhase("processing"), 350);
    try {
      const sourceMaterials = [
        ...(textSource ? [snapshotAsset(textSource, getDocumentMeta(textSource)?.text, "剧本/分镜原始资料")] : []),
        ...(guideAsset ? [snapshotAsset(guideAsset, undefined, "本地图片创作参考（不作为实际模型参考）")] : []),
        ...(videoGuideAsset ? [snapshotAsset(videoGuideAsset, undefined, "本地视频作品参考（不作为实际模型参考）")] : []),
      ];
      const combinedPrompt = shots.map((shot) => shot.prompt).join("\n\n---\n\n");
      const generatedAssets = await generateVideo(vid, {
        originalInput: combinedPrompt,
        generationInput: combinedPrompt,
        sourceMaterials,
        parentAssetIds: [...new Set([textSource?.asset.id, guideAsset?.asset.id, videoGuideAsset?.asset.id].filter((id): id is string => Boolean(id)))],
      });
      setSelectedVideoIds(generatedAssets.map((asset) => asset.id));
      setPhase("completed");
    } catch (cause) {
      setPhase("failed");
      setError(String(cause));
    } finally {
      if (processingTimer.current) clearTimeout(processingTimer.current);
      processingTimer.current = null;
      setBusy(false);
    }
  };

  const toggleVideoSelection = (id: string) => {
    setSelectedVideoIds((current) => current.includes(id) ? current.filter((item) => item !== id) : [...current, id]);
  };

  const joinSelected = async () => {
    if (joining || selectedVideoIds.length < 2) return;
    const selected = selectedVideoIds
      .map((id) => videos.find((item) => item.asset.id === id))
      .filter((item): item is LibAsset => Boolean(item));
    if (selected.length < 2) return;
    setJoining(true);
    setError(null);
    try {
      const output = await concatVideoAssets(selected);
      setSelectedVideoIds([]);
      const created = useLibraryStore.getState().assets.find((item) => item.asset.id === output.id);
      if (created) setPlaying(created);
    } catch (cause) {
      setError(`拼接失败：${String(cause)}`);
    } finally {
      setJoining(false);
    }
  };

  const reuseHistory = (asset: LibAsset) => {
    const params = asset.params ?? {};
    vid.load(params as Partial<VideoParams>);
    setTextSource(null);
    setGuideAsset(null);
    setVideoGuideAsset(null);
    setError(null);
    setPhase("idle");
  };

  const previewItem = videos[0];
  const preview = previewItem?.asset;
  const visibleImages = vid.mode !== "text" && capability.maxImages > 0;
  const visibleVideos = vid.mode === "reference" && capability.maxVideos > 0;
  const hasShotReferences = shots.some((shot) => (shot.referenceImages?.length ?? 0) + (shot.referenceVideos?.length ?? 0) + (shot.referenceLocalImages?.length ?? 0) > 0);
  const hasFirstFrameShot = shots.some((shot) => (shot.referenceStrategy ?? vid.mode) === "first_frame");

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-wrap items-center gap-2 border-b border-slate-800 bg-slate-900/40 px-6 py-2.5">
        <span className="text-xs font-medium text-slate-400">视频模型</span>
        <div className="w-80 max-w-full">
          <ModelCombobox
            compact
            label="视频模型"
            value={vid.model}
            options={modelOptions}
            labels={modelLabels}
            onChange={selectModel}
            onFetch={() => void videoCatalog.refresh()}
            fetching={videoCatalog.loading}
            status={videoCatalog.message ? { text: videoCatalog.message, error: videoCatalog.error } : null}
            placeholder="例如 kling-video-v3"
          />
        </div>
        <span className="text-[11px] text-slate-500">能力、上限与可选项按模型实时收敛；列表以外的模型需客户端内置能力定义后才能生成</span>
      </div>

      <WorkflowGuide current="检查每镜 Prompt 与模型支持的时长" next="逐镜生成并保存；需要时勾选镜头手动拼接" detail="每镜对应一次 API 请求和一个独立视频资源，不自动拆分或拼接。" />

      <div className="flex min-h-0 flex-1 gap-6 overflow-y-auto p-6">
        <section className="w-[360px] shrink-0 space-y-4 rounded-2xl border border-slate-800 bg-slate-900/60 p-5">
          <div className="flex items-center gap-2 text-sm font-semibold text-white">
            <span className="flex h-7 w-7 items-center justify-center rounded-lg bg-fuchsia-500"><Clapperboard size={15} /></span>
            能力驱动的视频创作
          </div>

          <div className="rounded-xl border border-fuchsia-300/10 bg-fuchsia-300/[0.04] px-3 py-2">
            <div className="text-xs text-fuchsia-100">{capability.label}</div>
            <p className="mt-1 text-[10px] leading-relaxed text-slate-400">{desktopCapabilitySummary(capability)}</p>
          </div>

          <Field label="生成方式" hint="仅显示当前模型支持的方式">
            <div className="grid grid-cols-3 gap-1 rounded-lg border border-slate-600 bg-slate-900/70 p-1">
              {capability.modes.map((mode) => (
                <button key={mode} type="button" onClick={() => selectMode(mode)} className={`rounded-md px-2 py-1.5 text-[11px] transition ${vid.mode === mode ? "bg-fuchsia-500 text-white" : "text-slate-400 hover:text-white"}`}>
                  {MODE_LABELS[mode]}
                </button>
              ))}
            </div>
          </Field>

          <Field label="视频镜头" hint={`${shots.length} 镜 · 共 ${shots.reduce((sum, shot) => sum + shot.durationS, 0)} 秒`}>
            <div className="space-y-2">
              {shots.map((shot, index) => {
                const shotMode = shot.referenceStrategy ?? vid.mode;
                const shotDurations = durationOptionsFor(capability, shotMode);
                return (
                  <div key={shot.id} className="rounded-xl border border-slate-700 bg-slate-950/30 p-2.5">
                    <div className="mb-1.5 flex items-center justify-between text-[10px] text-slate-400">
                      <div>
                        <span>第 {shot.shotNo} 镜</span>
                        <span aria-label={`第 ${shot.shotNo} 镜生成方式与参考素材`} className="ml-2 text-slate-500">
                          {MODE_LABELS[shotMode]} · {shotReferenceSummary(shot)}
                        </span>
                      </div>
                      <div className="flex items-center gap-2"><select aria-label={`第 ${shot.shotNo} 镜时长`} value={shot.durationS} onChange={(event) => updateShots(shots.map((item, itemIndex) => itemIndex === index ? { ...item, durationS: Number(event.target.value) } : item))} className="rounded border border-slate-700 bg-slate-900 px-2 py-1 text-[10px] text-slate-200">{shotDurations.map((duration) => <option key={duration} value={duration}>{duration} 秒</option>)}</select><button type="button" disabled={shots.length === 1} onClick={() => updateShots(shots.filter((_, itemIndex) => itemIndex !== index))} className="rounded p-1 text-slate-500 hover:text-rose-300 disabled:opacity-30" title="删除镜头"><Trash2 size={12} /></button></div>
                    </div>
                    <ShotPromptTextarea className={`${inputCls} h-20 resize-y leading-snug`} value={shot.prompt} placeholder="这一镜的完整视频 Prompt…" onCommit={(prompt) => commitShotPrompt(shot.id, prompt)} />
                    {((shot.referenceLocalImages?.length ?? 0) > 0 || (shot.referenceImages?.length ?? 0) > 0 || (shot.referenceVideos?.length ?? 0) > 0) && <div className="mt-2 space-y-1" aria-label={`第 ${shot.shotNo} 镜参考素材`}>
                      {(shot.referenceLocalImages ?? []).map((local) => <div key={local.assetId ?? local.sourceUri} className="flex items-center gap-2 rounded border border-cyan-300/10 bg-cyan-300/[0.03] p-1.5"><img src={convertFileSrc(local.path)} alt={local.label} className="h-8 w-8 rounded object-cover" /><span className="min-w-0 flex-1 truncate text-[10px] text-cyan-100">本地图片 · {local.label}<span className="ml-1 text-slate-500">随生成请求发送</span></span><button type="button" onClick={() => removeLocalImageReference(shot.id, local)} className="rounded p-1 text-slate-500 hover:text-rose-300" aria-label={`移除本地图片 ${local.label}`}><X size={11} /></button></div>)}
                      {(shot.referenceImages ?? []).map((url) => <div key={url} className="flex items-center gap-2 rounded border border-white/5 p-1.5"><span className="h-8 w-8 rounded bg-slate-800 text-center leading-8 text-[9px] text-slate-500">URL</span><span className="min-w-0 flex-1 truncate text-[10px] text-slate-400">托管图片 URL · {url}</span><button type="button" onClick={() => removeHostedImageReference(shot.id, url)} className="rounded p-1 text-slate-500 hover:text-rose-300" aria-label={`移除托管图片 ${url}`}><X size={11} /></button></div>)}
                      {(shot.referenceVideos ?? []).map((url) => <div key={url} className="flex items-center gap-2 rounded border border-fuchsia-300/10 bg-fuchsia-300/[0.03] p-1.5"><span className="h-8 w-8 rounded bg-slate-800 text-center leading-8 text-[9px] text-fuchsia-400">VIDEO</span><span className="min-w-0 flex-1 truncate text-[10px] text-fuchsia-200">托管视频 URL · {url}</span><button type="button" onClick={() => removeHostedVideoReference(shot.id, url)} className="rounded p-1 text-slate-500 hover:text-rose-300" aria-label={`移除托管视频 ${url}`}><X size={11} /></button></div>)}
                    </div>}
                  </div>
                );
              })}
            </div>
            <div className="mt-2 flex items-center justify-between gap-2">
              <button type="button" onClick={() => updateShots([...shots, { id: `shot-${Date.now()}`, shotNo: shots.length + 1, prompt: "", durationS: durationOptions[0] ?? capability.minDurationS }])} className="flex items-center gap-1 rounded-lg border border-fuchsia-300/15 px-2.5 py-1.5 text-[10px] text-fuchsia-100 hover:bg-fuchsia-300/5"><Plus size={11} /> 新增镜头</button>
              <div className="flex items-center gap-2"><select value={importTarget?.id ?? ""} onChange={(event) => setImportShotId(event.target.value)} aria-label="导入目标镜头" className="rounded border border-slate-700 bg-slate-900 px-2 py-1.5 text-[10px] text-slate-300">{shots.map((shot) => <option key={shot.id} value={shot.id}>导入到第 {shot.shotNo} 镜</option>)}</select><button type="button" onClick={() => setPickerOpen(true)} className="flex items-center gap-1 text-[10px] text-cyan-200/75 hover:text-white"><FileInput size={11} /> 从资产库导入</button><button type="button" onClick={() => void importExternalMedia().catch((cause) => setError(String(cause)))} className="flex items-center gap-1 text-[10px] text-cyan-200/75 hover:text-white"><Upload size={11} /> 上传到资产库</button></div>
            </div>
            {guideAsset && (
              <div className="mt-2 flex items-center gap-2 rounded-xl border border-cyan-300/10 bg-cyan-300/[0.035] p-2">
                <button type="button" onClick={() => setPlaying(guideAsset)} title="查看本地创作参考" className="shrink-0"><img src={convertFileSrc(guideAsset.asset.path)} alt={guideAsset.source} className="h-12 w-12 rounded-lg object-cover" /></button>
                <div className="min-w-0 flex-1"><div className="truncate text-[10px] text-cyan-100">本地创作参考：{guideAsset.source}</div><div className="mt-0.5 text-[9px] leading-snug text-slate-500">已提取画面描述帮助撰写提示词；当前视频服务仅支持公网 HTTPS 参考地址。</div></div>
                <button type="button" onClick={() => setGuideAsset(null)} className="rounded p-1 text-slate-500 hover:text-rose-300"><X size={12} /></button>
              </div>
            )}
            {videoGuideAsset && (
              <div className="mt-2 flex items-center gap-2 rounded-xl border border-fuchsia-300/10 bg-fuchsia-300/[0.035] p-2">
                <button type="button" onClick={() => setPlaying(videoGuideAsset)} title="查看本地视频作品参考" className="shrink-0"><video src={convertFileSrc(videoGuideAsset.asset.path)} muted className="h-12 w-20 rounded-lg object-cover" /></button>
                <div className="min-w-0 flex-1"><div className="truncate text-[10px] text-fuchsia-100">本地视频作品参考：{videoGuideAsset.source}</div><div className="mt-0.5 text-[9px] leading-snug text-slate-500">已导入创作链和溯源；当前视频服务仅支持公网 HTTPS 参考地址。</div></div>
                <button type="button" onClick={() => setVideoGuideAsset(null)} className="rounded p-1 text-slate-500 hover:text-rose-300"><X size={12} /></button>
              </div>
            )}
          </Field>

          {vid.mode === "text" ? (
            <div className="rounded-xl border border-slate-700 bg-slate-950/30 px-3 py-2 text-[10px] leading-relaxed text-slate-500">
              文生视频不携带参考素材。{hasMaterials ? "已填写的素材 URL 会保留在表单中；切换到参考模式后才可提交。" : "可从项目中导入本地图片；它会随生成请求发送给视频服务，或按选择经托管转为 URL。"}
            </div>
          ) : (
            <div className="space-y-3 rounded-xl border border-slate-800 bg-slate-950/25 p-3">
              <div className="text-[11px] font-medium text-slate-200">公共托管图片（未单独设置参考的镜头使用）</div>
              {hasShotReferences && <p className="text-[10px] text-slate-500">已设置逐镜参考的镜头使用上方素材。</p>}
              {visibleImages && <UrlListField label="公共托管图片 URL" values={images} max={vid.mode === "first_frame" ? 1 : capability.maxImages} onChange={(values) => setMaterials("images", values)} hint={vid.mode === "first_frame" ? "首帧最终本地图片 + URL 合计必须恰好 1 张" : capability.referenceImageCount ? `未单独设置参考的镜头必须恰好 ${capability.referenceImageCount} 张` : "每行一条"} />}
              {visibleVideos && <UrlListField label="参考视频 URL" values={vid.videos} max={capability.maxVideos} onChange={(values) => setMaterials("videos", values)} hint="每行一条" />}
              {capability.maxReferenceDurationS && vid.mode === "reference" && <p className="text-[10px] text-amber-200/80">参考图模式最长 {capability.maxReferenceDurationS} 秒。</p>}
            </div>
          )}

          <div className="grid grid-cols-2 gap-3">
            <Field label="画幅" hint={hasFirstFrameShot ? "首帧镜头由参考图比例决定" : undefined}>
              <select className={inputCls} value={vid.aspectRatio} onChange={(event) => vid.set({ aspectRatio: event.target.value })}>
                {capability.aspectRatios.map((ratio) => <option key={ratio} value={ratio}>{ratio}</option>)}
              </select>
            </Field>
            <Field label="分辨率">
              <select className={inputCls} value={vid.resolution} onChange={(event) => vid.set({ resolution: event.target.value })}>
                {capability.resolutions.map((resolution) => <option key={resolution} value={resolution}>{resolution}</option>)}
              </select>
            </Field>
          </div>

          {hasFirstFrameShot && <p className="-mt-1 text-[10px] leading-relaxed text-amber-200/75">首帧驱动镜头不会额外提交画幅参数，避免与参考图尺寸冲突；其他文生或多素材镜头仍使用上方画幅。</p>}

          <div className="rounded-xl border border-slate-800 bg-slate-950/25 px-3 py-2 text-[10px] leading-relaxed text-slate-500">声音、音频参考与字幕暂不处理；不会添加配音、混音、口型或字幕。</div>
          {vid.productionManifest && <div className="rounded-xl border border-cyan-300/15 bg-cyan-300/[0.05] px-3 py-2 text-xs text-cyan-100"><div className="font-medium">已加载审查通过的生产清单</div><div className="mt-1 text-[10px] text-cyan-100/70">模型 {vid.productionManifest.approvedModel} · 画幅 {vid.productionManifest.approvedAspectRatio} · 分辨率 {vid.productionManifest.approvedResolution} · 分镜 {vid.productionManifest.storyboardAssetId}</div><button type="button" className="mt-2 text-[10px] text-amber-200 hover:text-amber-100" onClick={() => vid.set({ productionManifest: undefined })}>解除审查清单，转为普通未审查视频任务</button></div>}

          {error && <div className="rounded-lg border border-rose-300/15 bg-rose-500/10 px-3 py-2 text-xs text-rose-200">{error}</div>}
          {notice && <div role="status" className="rounded-lg border border-cyan-300/15 bg-cyan-300/[0.06] px-3 py-2 text-xs text-cyan-100">{notice}</div>}
          {validationError && <div className="rounded-lg border border-amber-300/15 bg-amber-300/[0.06] px-3 py-2 text-xs text-amber-100">{validationError}</div>}

          <button type="button" onClick={() => void run()} disabled={Boolean(validationError) || busy} className="flex w-full items-center justify-center gap-2 rounded-lg bg-fuchsia-500 px-3 py-2.5 text-sm font-medium text-white transition hover:bg-fuchsia-400 disabled:cursor-not-allowed disabled:opacity-50">
            {busy ? <Loader2 size={15} className="animate-spin" /> : <Send size={15} />}
            {busy ? "正在逐镜生成…" : `生成 ${shots.length} 个视频镜头`}
          </button>
        </section>

        <section className="min-w-0 flex-1">
          <div className="flex h-full flex-col gap-4">
            <div className="rounded-2xl border border-slate-800 bg-slate-900/40 p-5">
              <div className="mb-3 flex items-center justify-between gap-4">
                <span className="text-xs font-semibold tracking-wide text-slate-300">生成工作流</span>
                <span className={`text-[10px] ${phase === "failed" ? "text-rose-300" : phase === "completed" ? "text-emerald-300" : "text-slate-500"}`}>
                  {phase === "idle" && "准备就绪"}
                  {phase === "submitting" && "正在提交任务"}
                  {phase === "processing" && "服务端排队 / 生成中"}
                  {phase === "completed" && "全部镜头已分别保存"}
                  {phase === "failed" && "任务失败，请修正后重试"}
                </span>
              </div>
              <div className="grid gap-2 sm:grid-cols-4">
                {["提交镜头", "逐镜生成", "逐镜下载", "分别保存"].map((label, index) => <div key={label} className={`rounded-lg border px-3 py-2 text-center text-[11px] ${workflowStepClass(phase, index)}`}>{label}</div>)}
              </div>
              {busy && <p className="mt-3 text-[10px] text-slate-500">视频服务以异步任务处理；每个镜头完成后立即作为独立资源保存，不自动拼接。</p>}
            </div>

            <div className="rounded-2xl border border-slate-800 bg-slate-900/40 p-5">
              <div className="mb-2 flex items-center justify-between text-xs font-semibold tracking-wide text-slate-400"><span>最新视频预览</span><span className="text-[10px] font-normal text-slate-600">仅显示当前项目的视频资产</span></div>
              <div className="flex min-h-[300px] items-center justify-center rounded-xl border border-dashed border-slate-700 bg-slate-800/30">
                {preview ? <div className="relative w-full"><video src={convertFileSrc(preview.path)} controls className="max-h-[60vh] w-full rounded-lg object-contain" /><button type="button" onClick={() => previewItem && setPlaying(previewItem)} className="absolute right-2 top-2 rounded-lg border border-white/10 bg-slate-950/75 px-2 py-1 text-[10px] text-cyan-200">完整详情</button></div> : busy ? <Loader2 size={24} className="animate-spin text-slate-500" /> : <div className="text-center text-sm text-slate-500">填写提示词并提交视频任务</div>}
              </div>
            </div>

            {videos.length > 0 && <div className="rounded-2xl border border-slate-800 bg-slate-900/40 p-5">
              <div className="mb-3 flex flex-wrap items-center justify-between gap-2"><div><span className="text-xs font-semibold tracking-wide text-slate-400">视频镜头与历史</span><span className="ml-2 text-[10px] text-slate-600">已选 {selectedVideoIds.length} 镜</span></div><button type="button" onClick={() => void joinSelected()} disabled={selectedVideoIds.length < 2 || joining} className="flex items-center gap-1.5 rounded-lg border border-fuchsia-300/20 bg-fuchsia-300/[0.06] px-3 py-1.5 text-[10px] text-fuchsia-100 disabled:cursor-not-allowed disabled:opacity-40">{joining ? <Loader2 size={11} className="animate-spin" /> : <Clapperboard size={11} />}{joining ? "拼接中…" : "拼接所选镜头"}</button></div>
              <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4">
                {videos.map((asset) => { const selected = selectedVideoIds.includes(asset.asset.id); return <article key={asset.asset.id} className={`overflow-hidden rounded-lg border bg-slate-800/60 ${selected ? "border-fuchsia-300/60 ring-1 ring-fuchsia-300/20" : "border-slate-700"}`}><div className="relative"><button type="button" onClick={() => setPlaying(asset)} onContextMenu={(event) => { event.preventDefault(); setMenu({ x: event.clientX, y: event.clientY, asset }); }} className="block aspect-video w-full"><video src={convertFileSrc(asset.asset.path)} muted title={`${asset.source}${asset.model ? ` · ${asset.model}` : ""}`} className="h-full w-full object-contain" /></button><button type="button" onClick={() => toggleVideoSelection(asset.asset.id)} className={`absolute left-2 top-2 flex h-6 w-6 items-center justify-center rounded-md border shadow ${selected ? "border-fuchsia-200 bg-fuchsia-500 text-white" : "border-white/20 bg-slate-950/75 text-transparent hover:text-slate-300"}`} title="选择用于拼接"><Check size={13} /></button></div><div className="border-t border-slate-700 p-2"><div className="truncate text-[10px] text-slate-300">{asset.source}</div><button type="button" onClick={() => reuseHistory(asset)} className="mt-1 text-[10px] text-cyan-200 hover:text-white">复用参数</button></div></article>; })}
              </div>
            </div>}
          </div>
        </section>
      </div>

      {menu && <ContextMenu x={menu.x} y={menu.y} onClose={() => setMenu(null)} items={[
        { label: "播放视频", icon: menuIcons.preview, onClick: () => setPlaying(menu.asset) },
        { label: "复用生成参数", icon: menuIcons.copy, onClick: () => reuseHistory(menu.asset) },
        { label: "复制到粘贴板", icon: menuIcons.copy, onClick: () => void navigator.clipboard?.writeText(menu.asset.asset.path).catch((cause) => logEvent("warn", "clipboard.write_failed", { error: String(cause) })) },
        { label: "打开所在文件夹", icon: menuIcons.open, onClick: () => void revealItemInDir(menu.asset.asset.path).catch((cause) => logEvent("warn", "asset.reveal_failed", { error: String(cause) })) },
      ]} />}

      <AssetDetailModal asset={playing} onClose={() => setPlaying(null)} onLoadAsset={playing?.asset.kind === "video" ? reuseHistory : undefined} />
      <AssetImportPicker open={pickerOpen} onClose={() => { pickerSession.current += 1; setPickerOpen(false); setPickerPreset(null); }} kinds={["text", "image", "video"]} actions={["prompt", "merge_prompt", "reference"]} strictProject title="导入当前项目的提示词或实际模型参考" onApply={applyImportedVideoAssets} entries={importEntries} initialEntries={pickerPreset?.entries} initialAction={pickerPreset?.action} onConfigureMediaHosting={() => setHostingDialogOpen(true)} mediaHosting={hostingStatus ? { configured: hostingStatus.configured, endpoint: hostingStatus.endpoint } : null} />
      <MediaHostingDialog open={hostingDialogOpen} onClose={() => setHostingDialogOpen(false)} onSaved={setHostingStatus} />
    </div>
  );
}
