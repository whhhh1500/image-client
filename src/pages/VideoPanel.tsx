import { useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Check, Clapperboard, FileInput, Loader2, Plus, Send, Trash2, X } from "lucide-react";
import { productionManifestMismatch, useVideoStore, type VideoGenerationItem, type VideoParams } from "../store/useVideoStore";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { concatVideoAssets, generateVideo } from "../lib/generateVideo";
import {
  listVideoModels,
  listVideoModelCapabilities,
  type VideoGenerationMode,
  type VideoModelCapability,
} from "../lib/ipc";
import { ContextMenu, menuIcons } from "../components/ContextMenu";
import AssetPicker from "../components/AssetPicker";
import AssetDetailModal from "../components/AssetDetailModal";
import { getDocumentMeta } from "../lib/documents";
import WorkflowGuide from "../components/WorkflowGuide";
import { logEvent } from "../lib/logger";
import { snapshotAsset } from "../lib/provenance";
import { parseStoryboardShots, storyboardShotsToGenerationItems, type StoryboardShot } from "../lib/video/storyboard";
import { isPublicHttpsUrl } from "../lib/video/referenceUrl";

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
    return {
      ...item,
      id: `shot-${item.shotNo}`,
      referenceImages: references.filter((asset) => asset.asset.kind === "image").map((asset) => asset.asset.path),
      referenceVideos: references.filter((asset) => asset.asset.kind === "video").map((asset) => asset.asset.path),
    };
  });
}

export function shotReferenceSummary(shot: VideoGenerationItem): string {
  const imageCount = shot.referenceImages?.length ?? 0;
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

export default function VideoPanel() {
  const vid = useVideoStore();
  const [capabilities, setCapabilities] = useState<VideoModelCapability[]>([]);
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [phase, setPhase] = useState<WorkflowPhase>("idle");
  const [selectedVideoIds, setSelectedVideoIds] = useState<string[]>([]);
  const [joining, setJoining] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number; asset: LibAsset } | null>(null);
  const [playing, setPlaying] = useState<LibAsset | null>(null);
  const [guideAsset, setGuideAsset] = useState<LibAsset | null>(null);
  const [textSource, setTextSource] = useState<LibAsset | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const processingTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const provider = useProjectStore();
  const defaultId = provider.projects[0]?.id;
  const activeId = provider.activeId;
  const belongs = (projectId?: string) => projectId === activeId || (!projectId && activeId === defaultId);
  const allAssets = useLibraryStore((state) => state.assets);
  // 视频工作区仅展示 video 资产；图片只可经“创作参考”显式导入，不会混入视频历史。
  const videos = allAssets.filter((item) => item.asset.kind === "video" && belongs(item.projectId));

  useEffect(() => {
    let disposed = false;
    void Promise.allSettled([listVideoModels(), listVideoModelCapabilities()])
      .then(([modelsResult, capabilitiesResult]) => {
        if (disposed) return;
        if (modelsResult.status === "fulfilled") setAvailableModels(modelsResult.value);
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

  const knownCapability = useMemo(
    () => capabilities.find((item) => item.id === vid.model),
    [capabilities, vid.model],
  );
  const capability = knownCapability ?? fallbackCapability(vid.model);
  const advertisedModels = availableModels.length ? availableModels : capabilities.map((item) => item.id);
  const modelOptions = [...new Set([vid.model, ...advertisedModels])]
    .filter(Boolean)
    .map((model) => capabilities.find((item) => item.id === model) ?? fallbackCapability(model));
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
      const unresolvedReferences = unresolvedShotReferenceIds(shot, allAssets, belongs);
      if (unresolvedReferences.length) return `第 ${shot.shotNo} 镜包含不可用的参考资产：${unresolvedReferences.join("、")}。参考资产必须是当前项目的图像或视频资源。`;
      const shotDurations = durationOptionsFor(capability, shotMode);
      if (!shotDurations.includes(shot.durationS)) return `${capability.label} 在 ${shotMode} 模式下不支持第 ${shot.shotNo} 镜的 ${shot.durationS} 秒时长。`;
      const shotImages = shot.referenceImages?.length ? shot.referenceImages : images;
      const shotVideos = shot.referenceVideos?.length ? shot.referenceVideos : vid.videos;
      const shotMaterials = [...shotImages, ...shotVideos];
      if (shotMaterials.some((value) => !isPublicHttpsUrl(value))) return `第 ${shot.shotNo} 镜的参考素材必须是无凭据的公网 HTTPS URL，不能使用本机或内网地址。`;
      if (shotImages.length > capability.maxImages || shotVideos.length > capability.maxVideos) return `第 ${shot.shotNo} 镜的参考素材超过当前模型上限。`;
      if (capability.maxTotalReferences && shotMaterials.length > capability.maxTotalReferences) return `第 ${shot.shotNo} 镜的参考素材总数超过当前模型上限（${shotMaterials.length}/${capability.maxTotalReferences}）。`;
      if (shotMode === "text" && (shot.referenceImages?.length || shot.referenceVideos?.length)) return `第 ${shot.shotNo} 镜为文生模式，不能携带逐镜参考素材。`;
      if (shotMode === "first_frame" && (shotImages.length !== 1 || shotVideos.length)) return `第 ${shot.shotNo} 镜的首帧模式需要且只能提供 1 张图片。`;
      if (shotMode === "reference" && !shotMaterials.length) return `第 ${shot.shotNo} 镜的多素材参考模式缺少参考素材。`;
      if (shotMode === "reference" && capability.referenceImageCount && shotImages.length !== capability.referenceImageCount) return `第 ${shot.shotNo} 镜必须恰好提供 ${capability.referenceImageCount} 张参考图片。`;
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
    if (vid.mode === "first_frame" && images.length !== 1) return "首帧驱动模式需要且只能提供 1 张图片 URL。";
    if (vid.mode === "reference" && !hasMaterials) return "多素材参考模式至少需要一条图片或视频 URL。";
    if (vid.mode === "reference" && capability.referenceImageCount && images.length !== capability.referenceImageCount) {
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

  const run = async () => {
    if (validationError || busy) return;
    setBusy(true);
    setError(null);
    setPhase("submitting");
    processingTimer.current = setTimeout(() => setPhase("processing"), 350);
    try {
      const sourceMaterials = [
        ...(textSource ? [snapshotAsset(textSource, getDocumentMeta(textSource)?.text, "剧本/分镜原始资料")] : []),
        ...(guideAsset ? [snapshotAsset(guideAsset, undefined, "本地图片创作参考（不上传）")] : []),
      ];
      const combinedPrompt = shots.map((shot) => shot.prompt).join("\n\n---\n\n");
      const generatedAssets = await generateVideo(vid, {
        originalInput: combinedPrompt,
        generationInput: combinedPrompt,
        sourceMaterials,
        parentAssetIds: [textSource?.asset.id, guideAsset?.asset.id].filter((id): id is string => Boolean(id)),
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
    setError(null);
    setPhase("idle");
  };

  const previewItem = videos[0];
  const preview = previewItem?.asset;
  const visibleImages = vid.mode !== "text" && capability.maxImages > 0;
  const visibleVideos = vid.mode === "reference" && capability.maxVideos > 0;
  const hasFirstFrameShot = shots.some((shot) => (shot.referenceStrategy ?? vid.mode) === "first_frame");

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-wrap items-center gap-2 border-b border-slate-800 bg-slate-900/40 px-6 py-2.5">
        <span className="text-xs font-medium text-slate-400">视频模型</span>
        <select value={vid.model} onChange={(event) => selectModel(event.target.value)} className="rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-1.5 text-xs text-slate-100 outline-none transition focus:border-fuchsia-400">
          {modelOptions.length ? modelOptions.map((item) => <option key={item.id} value={item.id}>{item.label}</option>) : <option value={vid.model}>{vid.model}</option>}
        </select>
        <span className="text-[11px] text-slate-500">能力、上限与可选项按模型实时收敛</span>
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
                    <textarea className={`${inputCls} h-20 resize-y leading-snug`} value={shot.prompt} placeholder="这一镜的完整视频 Prompt…" onChange={(event) => updateShots(shots.map((item, itemIndex) => itemIndex === index ? { ...item, prompt: event.target.value } : item))} />
                  </div>
                );
              })}
            </div>
            <div className="mt-2 flex items-center justify-between gap-2">
              <button type="button" onClick={() => updateShots([...shots, { id: `shot-${Date.now()}`, shotNo: shots.length + 1, prompt: "", durationS: durationOptions[0] ?? capability.minDurationS }])} className="flex items-center gap-1 rounded-lg border border-fuchsia-300/15 px-2.5 py-1.5 text-[10px] text-fuchsia-100 hover:bg-fuchsia-300/5"><Plus size={11} /> 新增镜头</button>
              <button type="button" onClick={() => setPickerOpen(true)} className="flex items-center gap-1 text-[10px] text-cyan-200/75 hover:text-white"><FileInput size={11} /> 导入创作参考</button>
            </div>
            {guideAsset && (
              <div className="mt-2 flex items-center gap-2 rounded-xl border border-cyan-300/10 bg-cyan-300/[0.035] p-2">
                <button type="button" onClick={() => setPlaying(guideAsset)} title="查看本地创作参考" className="shrink-0"><img src={convertFileSrc(guideAsset.asset.path)} alt={guideAsset.source} className="h-12 w-12 rounded-lg object-cover" /></button>
                <div className="min-w-0 flex-1"><div className="truncate text-[10px] text-cyan-100">本地创作参考：{guideAsset.source}</div><div className="mt-0.5 text-[9px] leading-snug text-slate-500">已提取画面描述帮助撰写提示词；本地图片不会上传，也不会伪装成 URL。</div></div>
                <button type="button" onClick={() => setGuideAsset(null)} className="rounded p-1 text-slate-500 hover:text-rose-300"><X size={12} /></button>
              </div>
            )}
          </Field>

          {vid.mode === "text" ? (
            <div className="rounded-xl border border-slate-700 bg-slate-950/30 px-3 py-2 text-[10px] leading-relaxed text-slate-500">
              文生视频不携带参考素材。{hasMaterials ? "已填写的素材 URL 会保留在表单中；切换到参考模式后才可提交。" : "可从项目中导入本地图片，仅用于提示词创作参考。"}
            </div>
          ) : (
            <div className="space-y-3 rounded-xl border border-slate-800 bg-slate-950/25 p-3">
              <div className="text-[11px] font-medium text-slate-200">公网参考素材</div>
              {visibleImages && <UrlListField label={vid.mode === "first_frame" ? "首帧图片 URL" : "参考图片 URL"} values={images} max={vid.mode === "first_frame" ? 1 : capability.maxImages} onChange={(values) => setMaterials("images", values)} hint={vid.mode === "first_frame" ? "必须恰好 1 张" : capability.referenceImageCount ? `必须恰好 ${capability.referenceImageCount} 张` : "每行一条"} />}
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
      <AssetPicker open={pickerOpen} onClose={() => setPickerOpen(false)} kinds={["text", "image"]} strictProject title="导入当前项目的剧本、分镜或图片资源" onPick={(asset) => {
        if (asset.asset.kind === "text") {
          const text = getDocumentMeta(asset)?.text || String(asset.params?.text ?? asset.source);
          const storyboard = parseStoryboardShots(text);
          const imported = storyboard.length
            ? resolveImportedStoryboardShots(storyboard, allAssets, belongs)
            : [{ id: `shot-${Date.now()}`, shotNo: 1, prompt: text, durationS: durationOptions[0] ?? capability.minDurationS }];
          updateShots(imported);
          if (storyboard.length) vid.set({ storyboardSourceAssetId: asset.asset.id });
          setTextSource(asset);
        } else if (asset.asset.kind === "image") {
          const sourcePrompt = typeof asset.params?.prompt === "string"
            ? asset.params.prompt
            : typeof (asset.params?.params as Record<string, unknown> | undefined)?.prompt === "string"
              ? String((asset.params?.params as Record<string, unknown>).prompt)
              : asset.source;
          const next = [...shots];
          next[0] = { ...next[0], prompt: [next[0]?.prompt.trim(), `本地图片创作参考：${sourcePrompt}`].filter(Boolean).join("\n\n") };
          updateShots(next);
          setGuideAsset(asset);
        }
      }} />
    </div>
  );
}
