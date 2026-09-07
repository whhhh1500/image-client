import { useEffect, useId, useRef, useState } from "react";
import { AlertTriangle, ArrowRight, Clapperboard, FileInput, GitBranch, History, Loader2, RotateCcw, Save, Sparkles } from "lucide-react";
import { agentLabel, agentSystem } from "../../store/useAgentStore";
import { useLibraryStore, type LibAsset } from "../../store/useLibraryStore";
import { useProjectStore } from "../../store/useProjectStore";
import { llmChat } from "../../lib/ipc";
import {
  documentChangeLabel,
  getDocumentMeta,
  getDocumentVersions,
  saveDocumentVersion,
  type DocumentChangeType,
  type DocumentType,
} from "../../lib/documents";
import { isStoryboardRoundTripSafe, parseStoryboardShots, serializeStoryboard, type StoryboardShot } from "../../lib/video/storyboard";
import {
  buildVideoOptimizationRequest,
  callVideoLlmWithSafetyRetry,
  cleanAndValidateVideoMarkdown,
  prepareVideoAgentContext,
  previousChapterAnchorAssets,
  VIDEO_REVIEW_POLICY_VERSION,
  videoOptimizationSystem,
  videoWorkflowIdForSource,
  type VideoMarkdownStage,
} from "../../lib/video/markdownWorkflow";
import { snapshotAsset } from "../../lib/provenance";
import AssetPicker from "../AssetPicker";
import {
  buildVideoQualityReviewRequest,
  deterministicVideoQualityIssues,
  parseVideoQcVerdict,
  parseVideoQualityReview,
  storyboardProductionIssues,
  videoQualityReviewSystem,
  type VideoQualityReviewStatus,
  type VideoQualityReviewSummary,
} from "../../lib/video/qualityReview";
import VideoDocumentViews, { type VideoDocumentView } from "./VideoDocumentViews";
import VideoQualityReviewCard from "./VideoQualityReviewCard";
import VideoStageNavigation, { videoResultStageStatus, type VideoStageStatus } from "./VideoStageNavigation";

type View = "source" | VideoMarkdownStage | "videos";
type SourceKind = "novel" | "idea";
type DraftChangeType = Exclude<DocumentChangeType, "copy">;
type DependencyMode = "current" | "view_history" | "preserve_history" | "migrate_latest";
type BusyAction = "generate" | "optimize" | "review" | "save";

interface StageDraft {
  text: string;
  optimizationInstruction: string;
  appliedOptimizationInstruction?: string;
  changeType: DraftChangeType;
  sourceKind?: SourceKind;
  sourceAssetId?: string;
  dependencyMode: DependencyMode;
  reviewMarkdown?: string;
  reviewStatus?: VideoQualityReviewStatus;
  reviewScore?: number | null;
  reviewedText?: string;
  reviewAt?: number;
  reviewPolicyVersion?: number;
}

const views: Array<{ id: View; label: string; agentId?: string; title?: string; documentType?: DocumentType }> = [
  { id: "source", label: "原始资料", title: "视频原始资料", documentType: "document" },
  { id: "director", label: "改编规划", agentId: "director", title: "导演规划", documentType: "director" },
  { id: "script", label: "剧本", agentId: "writer", title: "剧本", documentType: "script" },
  { id: "anchors", label: "视频锚点", agentId: "consistency", title: "视频锚点", documentType: "consistency" },
  { id: "storyboard", label: "视频分镜", agentId: "storyboard", title: "视频分镜", documentType: "storyboard" },
  { id: "qc", label: "质检", agentId: "qc", title: "视频质检", documentType: "qc" },
  { id: "videos", label: "视频结果" },
];
const videoOptimizationCascade: VideoMarkdownStage[] = ["director", "script", "anchors", "storyboard", "qc"];

const button = "rounded-lg border border-slate-700 px-3 py-2 text-xs text-slate-200 transition hover:border-cyan-300/30 hover:text-cyan-100 disabled:cursor-not-allowed disabled:opacity-40";
const primary = "rounded-lg bg-cyan-500 px-3 py-2 text-xs font-medium text-white transition hover:bg-cyan-400 disabled:cursor-not-allowed disabled:bg-slate-800 disabled:text-slate-500 disabled:opacity-70";
const field = "w-full rounded-xl border border-slate-700 bg-slate-950/60 px-3 py-2 text-sm text-slate-100 outline-none focus:border-cyan-300/50";

function stageSpec(view: View) {
  return views.find((item) => item.id === view);
}

function dialogFocusTargets(dialog: HTMLElement): HTMLElement[] {
  return [...dialog.querySelectorAll<HTMLElement>("a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])")]
    .filter((element) => !element.hasAttribute("hidden") && element.getAttribute("aria-hidden") !== "true");
}

function isStageAsset(asset: LibAsset, view: View): boolean {
  if (asset.asset.kind !== "text") return false;
  const meta = getDocumentMeta(asset);
  if (view === "source") return meta?.agentId === "source" || meta?.title.startsWith("视频原始资料") === true;
  return Boolean(stageSpec(view)?.agentId && meta?.agentId === stageSpec(view)?.agentId);
}

function isVideoSourceCandidate(asset: LibAsset): boolean {
  const meta = getDocumentMeta(asset);
  const sourceKind = asset.params?.sourceKind;
  return meta?.documentType === "novel" || meta?.agentId === "source" || sourceKind === "novel_chapter" || sourceKind === "idea";
}

function latest(assets: LibAsset[], projectId: string, view: View, workflowId?: string): LibAsset | undefined {
  return assets
    .filter((asset) => asset.projectId === projectId && isStageAsset(asset, view) && asset.params?.videoBranch !== true)
    .filter((asset) => {
      if (!workflowId) return true;
      const savedWorkflowId = typeof asset.params?.videoWorkflowId === "string" ? asset.params.videoWorkflowId : "";
      if (savedWorkflowId) return savedWorkflowId === workflowId;
      return view === "source" && getDocumentMeta(asset)?.documentId === workflowId;
    })
    .sort((a, b) => (getDocumentMeta(b)?.version ?? 0) - (getDocumentMeta(a)?.version ?? 0) || b.createdAt - a.createdAt)[0];
}

function dependencyViews(view: View): View[] {
  if (view === "director") return ["source"];
  if (view === "script") return ["source", "director"];
  if (view === "anchors") return ["source", "director", "script"];
  if (view === "storyboard") return ["source", "director", "script", "anchors"];
  if (view === "qc") return ["source", "director", "script", "anchors", "storyboard"];
  return [];
}

function workflowIdFromSource(asset?: LibAsset): string | undefined {
  if (!asset) return undefined;
  return typeof asset.params?.videoWorkflowId === "string"
    ? asset.params.videoWorkflowId
    : getDocumentMeta(asset)?.documentId;
}

function savedReview(asset?: LibAsset): Pick<StageDraft, "reviewMarkdown" | "reviewStatus" | "reviewScore" | "reviewedText" | "reviewAt" | "reviewPolicyVersion"> {
  if (!asset) return {};
  return {
    reviewMarkdown: typeof asset.params?.agentReview === "string" ? asset.params.agentReview : undefined,
    reviewStatus: asset.params?.agentReviewStatus === "passed" || asset.params?.agentReviewStatus === "needs_changes" ? asset.params.agentReviewStatus as VideoQualityReviewStatus : undefined,
    reviewScore: typeof asset.params?.agentReviewScore === "number" ? asset.params.agentReviewScore : undefined,
    reviewedText: typeof asset.params?.agentReviewedText === "string" ? asset.params.agentReviewedText : undefined,
    reviewAt: typeof asset.params?.agentReviewAt === "number" ? asset.params.agentReviewAt : undefined,
    reviewPolicyVersion: typeof asset.params?.agentReviewPolicyVersion === "number" ? asset.params.agentReviewPolicyVersion : undefined,
  };
}

function readDraft(key: string): StageDraft | undefined {
  try {
    const raw = localStorage.getItem(`video-md:draft:${key}`);
    if (!raw) return undefined;
    const value = JSON.parse(raw) as Partial<StageDraft>;
    if (typeof value.text !== "string") return undefined;
    return {
      text: value.text,
      optimizationInstruction: typeof value.optimizationInstruction === "string" ? value.optimizationInstruction : "",
      appliedOptimizationInstruction: typeof value.appliedOptimizationInstruction === "string" ? value.appliedOptimizationInstruction : undefined,
      changeType: value.changeType === "generated" || value.changeType === "ai_optimized" ? value.changeType : "manual",
      sourceKind: value.sourceKind === "idea" ? "idea" : value.sourceKind === "novel" ? "novel" : undefined,
      sourceAssetId: typeof value.sourceAssetId === "string" ? value.sourceAssetId : undefined,
      dependencyMode: ["current", "view_history", "preserve_history", "migrate_latest"].includes(String(value.dependencyMode)) ? value.dependencyMode as DependencyMode : "current",
      reviewMarkdown: typeof value.reviewMarkdown === "string" ? value.reviewMarkdown : undefined,
      reviewStatus: value.reviewStatus === "passed" || value.reviewStatus === "needs_changes" ? value.reviewStatus : undefined,
      reviewScore: typeof value.reviewScore === "number" ? value.reviewScore : undefined,
      reviewedText: typeof value.reviewedText === "string" ? value.reviewedText : undefined,
      reviewAt: typeof value.reviewAt === "number" ? value.reviewAt : undefined,
      reviewPolicyVersion: typeof value.reviewPolicyVersion === "number" ? value.reviewPolicyVersion : undefined,
    };
  } catch {
    return undefined;
  }
}

function writeDraft(key: string, draft?: StageDraft) {
  try {
    const storageKey = `video-md:draft:${key}`;
    if (draft) localStorage.setItem(storageKey, JSON.stringify(draft));
    else localStorage.removeItem(storageKey);
  } catch {
    // The in-memory draft remains usable when local storage is unavailable.
  }
}

function sourceLineage(asset?: LibAsset): Record<string, unknown> {
  if (!asset) return {};
  const keys = ["sourceKind", "sourceAssetId", "novelWorkId", "novelChapterId", "novelChapterRevisionId", "chapterNo"];
  return Object.fromEntries(keys.flatMap((key) => asset.params?.[key] === undefined ? [] : [[key, asset.params[key]]]));
}

function chapterLabel(asset?: LibAsset): string {
  const chapterNo = asset?.params?.chapterNo;
  if (typeof chapterNo === "number") return `第${chapterNo}章`;
  return asset?.params?.sourceKind === "idea" ? "脑洞" : "当前资料";
}

function productionReviewStatus(asset?: LibAsset): VideoQualityReviewStatus | undefined {
  const meta = asset ? getDocumentMeta(asset) : null;
  const review = savedReview(asset);
  if (!review.reviewMarkdown || review.reviewedText !== meta?.text || review.reviewPolicyVersion !== VIDEO_REVIEW_POLICY_VERSION) return undefined;
  try {
    const parsed = parseVideoQualityReview(review.reviewMarkdown);
    return parsed.status === "passed" && review.reviewStatus === "passed" ? "passed" : "needs_changes";
  } catch {
    return "needs_changes";
  }
}

function draftDiffersFromSaved(draft: StageDraft, asset: LibAsset | undefined, view: View): boolean {
  const meta = asset ? getDocumentMeta(asset) : null;
  const saved = savedReview(asset);
  const savedSourceKind: SourceKind = asset?.params?.sourceKind === "idea" ? "idea" : "novel";
  return draft.text !== (meta?.text ?? "")
    || (view === "source" && (draft.sourceKind ?? "novel") !== savedSourceKind)
    || draft.dependencyMode === "preserve_history"
    || draft.dependencyMode === "migrate_latest"
    || draft.reviewMarkdown !== saved.reviewMarkdown
    || draft.reviewStatus !== saved.reviewStatus
    || draft.reviewedText !== saved.reviewedText;
}

function qcBindings(stageAsset: (view: View) => LibAsset | undefined, approvedModel?: string, approvedAspectRatio?: string, approvedResolution?: string): Record<string, unknown> {
  return {
    checkedSourceAssetId: stageAsset("source")?.asset.id,
    checkedDirectorAssetId: stageAsset("director")?.asset.id,
    checkedScriptAssetId: stageAsset("script")?.asset.id,
    checkedAnchorAssetId: stageAsset("anchors")?.asset.id,
    checkedStoryboardAssetId: stageAsset("storyboard")?.asset.id,
    checkedVideoModel: approvedModel,
    checkedAspectRatio: approvedAspectRatio,
    checkedResolution: approvedResolution,
  };
}

export default function VideoMarkdownWorkspace({ llmModel, onEditPrompt, onSendToVideo }: {
  llmModel?: string;
  onEditPrompt: (id: string) => void;
  onSendToVideo: (shots: StoryboardShot[], source: LibAsset, approval: { anchorAssetId?: string; qcAssetId?: string; approvedModel: string; approvedAspectRatio: string; approvedResolution: string }) => void;
}) {
  const projectId = useProjectStore((state) => state.activeId);
  const projects = useProjectStore((state) => state.projects);
  const assets = useLibraryStore((state) => state.assets);
  const tasks = useLibraryStore((state) => state.tasks);
  const [view, setView] = useState<View>("source");
  const [selectedByStage, setSelectedByStage] = useState<Record<string, string | undefined>>({});
  const [drafts, setDrafts] = useState<Record<string, StageDraft>>({});
  const [documentViews, setDocumentViews] = useState<Record<string, VideoDocumentView>>({});
  const [activeWorkflowId, setActiveWorkflowId] = useState<string>();
  const [busy, setBusy] = useState<BusyAction | null>(null);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [pickerOpen, setPickerOpen] = useState(false);
  const [discardConfirmOpen, setDiscardConfirmOpen] = useState(false);
  const stageTabsId = useId();
  const discardDialogRef = useRef<HTMLElement | null>(null);
  const discardCancelRef = useRef<HTMLButtonElement | null>(null);
  const discardReturnFocusRef = useRef<HTMLButtonElement | null>(null);
  const discardDialogWasOpenRef = useRef(false);

  const closeDiscardDialog = () => setDiscardConfirmOpen(false);
  const openDiscardDialog = (trigger: HTMLButtonElement) => {
    discardReturnFocusRef.current = trigger;
    setDiscardConfirmOpen(true);
  };

  useEffect(() => {
    if (discardConfirmOpen) {
      discardDialogWasOpenRef.current = true;
      const frame = window.requestAnimationFrame(() => discardCancelRef.current?.focus());
      return () => window.cancelAnimationFrame(frame);
    }
    if (discardDialogWasOpenRef.current) {
      discardDialogWasOpenRef.current = false;
      discardReturnFocusRef.current?.focus();
    }
  }, [discardConfirmOpen]);

  useEffect(() => {
    try {
      setActiveWorkflowId(projectId ? localStorage.getItem(`video-md:last-workflow:${projectId}`) ?? undefined : undefined);
    } catch {
      setActiveWorkflowId(undefined);
    }
    setSelectedByStage({});
    setMessage("");
    setError("");
  }, [projectId]);

  if (!projectId) return <div className="p-8 text-sm text-slate-400">请先选择或新建项目。</div>;

  const project = projects.find((item) => item.id === projectId);
  const workspaceSources = [...assets]
    .filter((asset) => asset.projectId === projectId && isStageAsset(asset, "source") && asset.params?.videoBranch !== true)
    .sort((a, b) => b.createdAt - a.createdAt)
    .reduce<LibAsset[]>((result, asset) => {
      const id = workflowIdFromSource(asset);
      if (id && !result.some((item) => workflowIdFromSource(item) === id)) result.push(asset);
      return result;
    }, []);
  const rememberedWorkflowId = activeWorkflowId && workspaceSources.some((asset) => workflowIdFromSource(asset) === activeWorkflowId) ? activeWorkflowId : undefined;
  const requiresWorkspaceSelection = workspaceSources.length > 1 && !rememberedWorkflowId;
  const workflowId = rememberedWorkflowId ?? (workspaceSources.length === 1 ? workflowIdFromSource(workspaceSources[0]) : undefined) ?? `video:idea:${projectId}`;
  const chooseWorkflow = (id: string) => {
    setActiveWorkflowId(id);
    try { localStorage.setItem(`video-md:last-workflow:${projectId}`, id); } catch { /* in-memory selection remains available */ }
    setView("source");
    setMessage("已切换视频工作区；其他工作区草稿仍保留。");
    setError("");
  };
  const stageAsset = (id: View) => latest(assets, projectId, id, workflowId);
  const sourceAsset = stageAsset("source");
  const lineage = sourceLineage(sourceAsset);
  const novelWorkId = typeof lineage.novelWorkId === "string" ? lineage.novelWorkId : undefined;
  const chapterNo = typeof lineage.chapterNo === "number" ? lineage.chapterNo : undefined;
  const novelChapterId = typeof lineage.novelChapterId === "string" ? lineage.novelChapterId : undefined;
  const latestPublishedChapter = novelChapterId ? assets
    .filter((asset) => asset.projectId === projectId && asset.asset.kind === "text")
    .filter((asset) => getDocumentMeta(asset)?.documentType === "novel" && asset.params?.novelChapterId === novelChapterId)
    .sort((left, right) => right.createdAt - left.createdAt)[0] : undefined;
  const savedChapterRevisionId = typeof sourceAsset?.params?.novelChapterRevisionId === "string" ? sourceAsset.params.novelChapterRevisionId : "";
  const latestChapterRevisionId = typeof latestPublishedChapter?.params?.novelChapterRevisionId === "string" ? latestPublishedChapter.params.novelChapterRevisionId : "";
  const sourceOutdated = Boolean(sourceAsset && latestPublishedChapter && (savedChapterRevisionId && latestChapterRevisionId
    ? savedChapterRevisionId !== latestChapterRevisionId
    : sourceAsset.params?.sourceAssetId !== latestPublishedChapter.asset.id));
  const inheritedAnchors = previousChapterAnchorAssets(assets, { projectId, novelWorkId, chapterNo, limit: 3 });
  const dependencyAssets = (id: View): LibAsset[] => [
    ...dependencyViews(id).map(stageAsset).filter((asset): asset is LibAsset => Boolean(asset)),
    ...(id === "anchors" ? inheritedAnchors : []),
  ];
  const stale = (id: View) => {
    const document = stageAsset(id);
    if (!document || id === "videos") return false;
    if (id === "source") return sourceOutdated;
    const requiredViews = dependencyViews(id);
    if (requiredViews.some((dependency) => !stageAsset(dependency))) return true;
    if (requiredViews.some((dependency) => stale(dependency))) return true;
    const parents = new Set(getDocumentMeta(document)?.provenance?.parentAssetIds ?? []);
    return dependencyAssets(id).some((asset) => !parents.has(asset.asset.id));
  };
  const stageUsable = (id: View) => {
    const asset = stageAsset(id);
    if (!asset || stale(id)) return false;
    return id === "source" || productionReviewStatus(asset) === "passed";
  };

  const current = stageAsset(view);
  const stageKey = `${projectId}:${workflowId}:${view}`;
  const selectedAssetId = selectedByStage[stageKey] ?? current?.asset.id;
  const selectedAsset = assets.find((asset) => asset.asset.id === selectedAssetId && isStageAsset(asset, view));
  const historical = Boolean(selectedAsset && current && selectedAsset.asset.id !== current.asset.id);
  const editorKey = `${stageKey}:${selectedAssetId ?? "new"}`;
  const selectedMeta = selectedAsset ? getDocumentMeta(selectedAsset) : null;
  const storedDraft = drafts[editorKey] ?? readDraft(editorKey);
  const activeDraft: StageDraft = storedDraft ?? {
    text: selectedMeta?.text ?? "",
    optimizationInstruction: selectedMeta?.provenance?.revision?.instruction ?? "",
    changeType: "manual",
    sourceKind: selectedAsset?.params?.sourceKind === "idea" ? "idea" : "novel",
    sourceAssetId: typeof selectedAsset?.params?.sourceAssetId === "string" ? selectedAsset.params.sourceAssetId : undefined,
    dependencyMode: historical ? "view_history" : "current",
    ...savedReview(selectedAsset),
  };
  const sourceParent = assets.find((asset) => asset.asset.id === activeDraft.sourceAssetId);
  const savedSourceKind: SourceKind = selectedAsset?.params?.sourceKind === "idea" ? "idea" : "novel";
  const contentDirty = activeDraft.text !== (selectedMeta?.text ?? "")
    || (view === "source" && (activeDraft.sourceKind ?? "novel") !== savedSourceKind)
    || activeDraft.dependencyMode === "preserve_history"
    || activeDraft.dependencyMode === "migrate_latest";
  const selectedSavedReview = savedReview(selectedAsset);
  const reviewValid = Boolean(activeDraft.reviewMarkdown && activeDraft.reviewedText === activeDraft.text);
  const activeReviewStatus = reviewValid ? activeDraft.reviewStatus : undefined;
  const reviewDirty = activeDraft.reviewMarkdown !== selectedSavedReview.reviewMarkdown
    || activeDraft.reviewStatus !== selectedSavedReview.reviewStatus
    || activeDraft.reviewedText !== selectedSavedReview.reviewedText;
  const dirty = contentDirty || reviewDirty;
  const versions = current ? getDocumentVersions(current, assets) : selectedAsset ? getDocumentVersions(selectedAsset, assets) : [];
  const storyboardShots = view === "storyboard" ? parseStoryboardShots(activeDraft.text) : [];
  const currentDocumentView = documentViews[stageKey] ?? (view === "anchors" || view === "storyboard" ? "overview" : "markdown");
  const storyboardAsset = stageAsset("storyboard");
  const anchorAsset = stageAsset("anchors");
  const qcAsset = stageAsset("qc");
  const productionIssues = storyboardAsset ? storyboardProductionIssues({
    storyboardMarkdown: getDocumentMeta(storyboardAsset)?.text ?? "",
    anchorMarkdown: anchorAsset ? getDocumentMeta(anchorAsset)?.text ?? "" : "",
    expectedAspectRatio: project?.aspectRatio,
    availableAssets: assets.filter((asset) => asset.projectId === projectId).map((asset) => ({ id: asset.asset.id, kind: asset.asset.kind, path: asset.asset.path })),
    sourceContext: sourceAsset ? getDocumentMeta(sourceAsset)?.text ?? "" : "",
  }) : ["尚未保存视频分镜"];
  const qcVerdict = parseVideoQcVerdict(qcAsset ? getDocumentMeta(qcAsset)?.text ?? "" : "");
  const expectedQcBindings = qcBindings(stageAsset, project?.videoModel, project?.aspectRatio, project?.videoResolution || "720p");
  const qcBindingMatches = Boolean(qcAsset && Object.entries(expectedQcBindings).every(([key, value]) => value && qcAsset.params?.[key] === value));
  const qcReviewPassed = productionReviewStatus(qcAsset) === "passed";
  const gateReasons = [
    ...(!storyboardAsset ? ["尚未保存视频分镜"] : stale("storyboard") ? ["视频分镜依赖已过期"] : []),
    ...productionIssues,
    ...(!qcAsset ? ["尚未保存质检报告"] : stale("qc") ? ["质检报告检查的不是当前依赖版本"] : []),
    ...(qcAsset && !qcVerdict.passed ? [`质检结论未通过：${qcVerdict.blockingIssues || qcVerdict.conclusion}`] : []),
    ...(qcAsset && !qcBindingMatches ? ["质检报告没有精确绑定当前原始资料、规划、剧本、锚点和分镜"] : []),
    ...(qcAsset && !qcReviewPassed ? ["质检报告尚未通过独立质量复核"] : []),
  ].filter((reason, index, all) => reason && all.indexOf(reason) === index);
  const storyboardReady = gateReasons.length === 0;
  const videos = assets
    .filter((asset) => asset.projectId === projectId && asset.asset.kind === "video" && asset.params?.storyboardSourceAssetId === storyboardAsset?.asset.id)
    .sort((a, b) => b.createdAt - a.createdAt);
  const savedStoryboardShots = parseStoryboardShots(storyboardAsset ? getDocumentMeta(storyboardAsset)?.text ?? "" : "");
  const generatedShotNos = new Set(videos.map((asset) => Number(asset.params?.shotNo)).filter((shotNo) => Number.isSafeInteger(shotNo) && shotNo > 0));
  const completedShotCount = savedStoryboardShots.filter((shot) => generatedShotNos.has(shot.shotNo)).length;
  const failedVideoTasks = tasks.filter((task) => task.projectId === projectId && task.kind === "video" && task.status === "error" && task.params?.storyboardSourceAssetId === storyboardAsset?.asset.id);

  const stageDraftEntries = (id: View): Array<{ draft: StageDraft; asset?: LibAsset }> => {
    if (id === "videos") return [];
    const prefix = `${projectId}:${workflowId}:${id}:`;
    const entries = new Map<string, StageDraft>();
    Object.entries(drafts).forEach(([key, draft]) => { if (key.startsWith(prefix)) entries.set(key, draft); });
    try {
      for (let index = 0; index < localStorage.length; index += 1) {
        const storageKey = localStorage.key(index);
        const marker = `video-md:draft:${prefix}`;
        if (!storageKey?.startsWith(marker)) continue;
        const editorKey = storageKey.slice("video-md:draft:".length);
        if (!entries.has(editorKey)) {
          const stored = readDraft(editorKey);
          if (stored) entries.set(editorKey, stored);
        }
      }
    } catch {
      // In-memory drafts remain authoritative when local storage is unavailable.
    }
    return [...entries.entries()].map(([key, draft]) => {
      const assetId = key.slice(prefix.length);
      return { draft, asset: assetId === "new" ? undefined : assets.find((asset) => asset.asset.id === assetId) };
    });
  };
  const stageHasUnsavedDraft = (id: View) => stageDraftEntries(id).some(({ draft, asset }) => draftDiffersFromSaved(draft, asset, id));

  const persistDraft = (next: StageDraft) => {
    if (selectedAssetId) {
      setSelectedByStage((state) => state[stageKey] === selectedAssetId ? state : { ...state, [stageKey]: selectedAssetId });
    }
    setDrafts((state) => ({ ...state, [editorKey]: next }));
    writeDraft(editorKey, next);
  };
  const updateDraft = (patch: Partial<StageDraft>, invalidateReview = false) => {
    persistDraft({
      ...activeDraft,
      ...patch,
      ...(invalidateReview ? { reviewMarkdown: undefined, reviewStatus: undefined, reviewScore: undefined, reviewedText: undefined, reviewAt: undefined, reviewPolicyVersion: undefined } : {}),
    });
  };
  const loadLatestChapterRevision = () => {
    if (!latestPublishedChapter) return;
    updateDraft({
      text: getDocumentMeta(latestPublishedChapter)?.text ?? String(latestPublishedChapter.params?.text ?? ""),
      sourceKind: "novel",
      sourceAssetId: latestPublishedChapter.asset.id,
      changeType: "manual",
      dependencyMode: "current",
    }, true);
    setDocumentView("compare");
    setMessage(`已载入章节最新版：${latestPublishedChapter.source}。当前只是迁移草稿，保存后下游才会按新正文进入更新流程。`);
  };
  const setDocumentView = (next: VideoDocumentView) => setDocumentViews((state) => ({ ...state, [stageKey]: next }));

  const selectVersion = (assetId: string) => {
    setSelectedByStage((state) => ({ ...state, [stageKey]: assetId }));
    setMessage("已打开保存版本。历史版本默认只读，需明确选择‘历史分支’或‘迁移最新依赖’后才能修改。");
    setError("");
  };

  const beginHistoricalDraft = (mode: "preserve_history" | "migrate_latest") => {
    persistDraft({
      ...activeDraft,
      dependencyMode: mode,
      ...(mode === "migrate_latest" ? { reviewMarkdown: undefined, reviewStatus: undefined, reviewScore: undefined, reviewedText: undefined, reviewAt: undefined, reviewPolicyVersion: undefined } : {}),
    });
    setDocumentView("compare");
    setMessage(mode === "preserve_history"
      ? "已创建历史分支草稿：保存时保留旧依赖，不会替代当前生产版本。"
      : "已创建迁移草稿：内容尚未证明适配最新依赖，请先审查或使用 LLM 优化后再保存。");
  };

  const confirmDiscard = () => {
    const restored: StageDraft = {
      text: selectedMeta?.text ?? "",
      optimizationInstruction: selectedMeta?.provenance?.revision?.instruction ?? "",
      changeType: "manual",
      sourceKind: selectedAsset?.params?.sourceKind === "idea" ? "idea" : "novel",
      sourceAssetId: typeof selectedAsset?.params?.sourceAssetId === "string" ? selectedAsset.params.sourceAssetId : undefined,
      dependencyMode: historical ? "view_history" : "current",
      ...savedReview(selectedAsset),
    };
    setDrafts((state) => ({ ...state, [editorKey]: restored }));
    writeDraft(editorKey, undefined);
    closeDiscardDialog();
    setMessage("已放弃当前草稿并恢复所选保存版本，其他阶段草稿不受影响。");
    setError("");
  };

  const projectContext = `项目：${project?.name ?? ""}\n题材：${project?.storyStyle ?? ""}\n画风：${project?.artStyle ?? ""}\n画幅：${project?.aspectRatio ?? ""}\n视频模型：${project?.videoModel ?? ""}\n视频分辨率：${project?.videoResolution || "720p"}\n声音、配音、口型、字幕：本阶段不做\n视频拼接：仅用户点击拼接按钮时执行`;

  const requiredDependencies = (id: View) => dependencyViews(id).map(stageAsset);
  const dependenciesReady = (id: View) => requiredDependencies(id).every(Boolean)
    && dependencyViews(id).every((dependency) => stageUsable(dependency));
  const blockedDependencies = dependencyViews(view).filter((id) => !stageUsable(id));
  const dependencyContext = (id: View) => dependencyAssets(id)
    .map((asset) => {
      const priorChapter = id === "anchors" && asset.params?.chapterNo !== chapterNo;
      return `# ${priorChapter ? `上章已确认锚点 · ${chapterLabel(asset)}` : asset.source}\n\n${getDocumentMeta(asset)?.text ?? ""}`;
    })
    .join("\n\n---\n\n");
  const preservedDependencyContext = () => (selectedMeta?.provenance?.sourceMaterials ?? [])
    .map((material) => `# ${material.label}\n\n${material.text ?? ""}`)
    .join("\n\n---\n\n");
  const reviewDependencyContext = activeDraft.dependencyMode === "preserve_history" ? preservedDependencyContext() : dependencyContext(view);
  const optimizationWorkspaceContext = (target: View, workingTexts: Partial<Record<View, string>> = {}) => {
    const documents = views
      .filter((item) => item.id !== "videos" && item.id !== target)
      .map((item) => ({ item, asset: stageAsset(item.id) }))
      .filter((entry): entry is { item: (typeof views)[number]; asset: LibAsset } => Boolean(entry.asset))
      .map(({ item, asset }) => {
        const meta = getDocumentMeta(asset);
        const state = item.id === "source"
          ? stale(item.id) ? "需更新" : "当前已保存版本"
          : stale(item.id) ? "需更新，不能覆盖权威上游"
            : productionReviewStatus(asset) === "passed" ? "已审查通过"
              : productionReviewStatus(asset) === "needs_changes" ? "审查需修改"
                : "尚未通过独立审查";
        const workingText = workingTexts[item.id];
        return `# ${item.label} · ${workingText !== undefined ? "本次联动新版本" : `v${meta?.version ?? 1}`} · ${workingText !== undefined ? "正在联动更新" : state}\n\n${workingText ?? meta?.text ?? ""}`;
      });
    const storyboard = stageAsset("storyboard");
    const referenceIds = new Set(
      parseStoryboardShots(storyboard ? getDocumentMeta(storyboard)?.text ?? "" : "")
        .flatMap((shot) => shot.referenceAssetIds),
    );
    const relatedMedia = assets
      .filter((asset) => asset.projectId === projectId && (asset.asset.kind === "image" || asset.asset.kind === "video"))
      .filter((asset) => referenceIds.has(asset.asset.id)
        || asset.asset.kind === "video" && storyboard && asset.params?.storyboardSourceAssetId === storyboard.asset.id)
      .sort((left, right) => left.createdAt - right.createdAt)
      .map((asset) => {
        const shotNo = typeof asset.params?.shotNo === "number" ? ` · 第${asset.params.shotNo}镜` : "";
        const duration = asset.asset.durationS ?? (typeof asset.params?.shotDurationS === "number" ? asset.params.shotDurationS : undefined);
        const providerTaskId = typeof asset.params?.providerTaskId === "string" ? ` · Provider任务 ${asset.params.providerTaskId}` : "";
        return `- ${asset.asset.kind === "image" ? "图像参考" : "视频结果"}${shotNo}：${asset.source} · 资产 ${asset.asset.id}${asset.model ? ` · 模型 ${asset.model}` : ""}${duration ? ` · ${duration}秒` : ""}${providerTaskId}`;
      });
    return [
      ...documents,
      `# 关联媒体资源（只提供元数据，文本模型不能修改媒体）\n\n${relatedMedia.length ? relatedMedia.join("\n") : "无"}`,
    ].join("\n\n---\n\n");
  };

  const currentLineage = (): Record<string, unknown> => {
    const parent = sourceParent ?? sourceAsset;
    return {
      videoWorkflowId: workflowId,
      ...sourceLineage(parent),
      sourceKind: activeDraft.sourceKind ?? parent?.params?.sourceKind ?? "idea",
      sourceAssetId: activeDraft.sourceAssetId ?? parent?.params?.sourceAssetId,
    };
  };

  const reviewOutput = async (stage: VideoMarkdownStage, markdown: string, options?: {
    dependencyContext?: string;
    workingTexts?: Partial<Record<View, string>>;
  }): Promise<VideoQualityReviewSummary> => {
    const effectiveDependencyContext = options?.dependencyContext ?? reviewDependencyContext;
    const deterministicIssues = stage === "storyboard"
      ? storyboardProductionIssues({
          storyboardMarkdown: markdown,
          anchorMarkdown: options?.workingTexts?.anchors ?? getDocumentMeta(stageAsset("anchors")!)?.text ?? "",
          expectedAspectRatio: project?.aspectRatio,
          availableAssets: assets.filter((asset) => asset.projectId === projectId).map((asset) => ({ id: asset.asset.id, kind: asset.asset.kind, path: asset.asset.path })),
          sourceContext: options?.workingTexts?.source ?? (sourceAsset ? getDocumentMeta(sourceAsset)?.text ?? "" : ""),
        })
      : deterministicVideoQualityIssues(stage, markdown, project?.aspectRatio, effectiveDependencyContext);
    const raw = await callVideoLlmWithSafetyRetry(
      (system, user) => llmChat(system, user, llmModel),
      videoQualityReviewSystem(stage),
      prepareVideoAgentContext(buildVideoQualityReviewRequest({ stage, markdown, projectContext, dependencyContext: effectiveDependencyContext, deterministicIssues })),
    );
    const review = parseVideoQualityReview(raw);
    if (!deterministicIssues.length) return review;
    return {
      ...review,
      status: "needs_changes",
      markdown: `${review.markdown}\n\n【程序阻断】\n${deterministicIssues.map((issue, index) => `${index + 1}. ${issue}`).join("\n")}`,
      blockingIssues: [review.blockingIssues, ...deterministicIssues].filter(Boolean).join("\n"),
    };
  };

  const save = async () => {
    if (!activeDraft.text.trim() || busy) return;
    const spec = stageSpec(view);
    if (!spec?.documentType || !spec.title) return;
    if (historical && (activeDraft.dependencyMode === "view_history" || activeDraft.dependencyMode === "current")) {
      setError(activeDraft.dependencyMode === "current"
        ? "编辑期间出现了新的当前生产版。请选择保留原依赖为历史分支，或迁移到最新依赖后再保存。"
        : "历史版本默认只读。请选择按原依赖创建分支，或迁移到最新依赖。");
      return;
    }
    const preservingBranch = activeDraft.dependencyMode === "preserve_history";
    if (view !== "source" && !preservingBranch && !dependenciesReady(view)) {
      setError(`当前阶段被前序质量门阻塞：${blockedDependencies.map((id) => stageSpec(id)?.label).join("、") || "请检查依赖"}`);
      return;
    }
    setBusy("save");
    setError("");
    setMessage("");
    try {
      const sourceKind = activeDraft.sourceKind ?? "novel";
      const body = view === "source"
        ? `# 原始资料\n\n## 类型\n${sourceKind === "novel" ? "小说" : "脑洞"}\n\n## 正文\n${activeDraft.text.replace(/^# 原始资料[\s\S]*?## 正文\s*/u, "").trim()}`
        : activeDraft.text.trim();
      if (view === "storyboard" && !parseStoryboardShots(body).length) {
        throw new Error("视频分镜 Markdown 字段不完整，无法保存");
      }
      const latestDependencies = dependencyAssets(view);
      const preservedParentIds = selectedMeta?.provenance?.parentAssetIds ?? [];
      const preservedMaterials = selectedMeta?.provenance?.sourceMaterials ?? [];
      const dependencies = preservingBranch
        ? preservedParentIds.map((id) => assets.find((asset) => asset.asset.id === id)).filter((asset): asset is LibAsset => Boolean(asset))
        : latestDependencies;
      const metadata: Record<string, unknown> = {
        ...currentLineage(),
        videoBranch: preservingBranch,
        dependencyMode: preservingBranch ? "preserve_history" : "current",
        ...(reviewValid ? {
          agentReview: activeDraft.reviewMarkdown,
          agentReviewStatus: activeDraft.reviewStatus,
          agentReviewScore: activeDraft.reviewScore,
          agentReviewedText: body,
          agentReviewAt: activeDraft.reviewAt ?? Date.now(),
          agentReviewPolicyVersion: activeDraft.reviewPolicyVersion,
        } : { agentReviewStatus: "pending" }),
        ...(view === "qc" && !preservingBranch ? qcBindings(stageAsset, project?.videoModel, project?.aspectRatio, project?.videoResolution || "720p") : {}),
      };
      const titleSuffix = typeof metadata.chapterNo === "number" ? ` · 第${metadata.chapterNo}章` : "";
      const saved = await saveDocumentVersion({
        title: `${spec.title}${titleSuffix}${preservingBranch ? " · 历史分支" : ""}`,
        text: body,
        model: llmModel,
        projectId,
        documentType: spec.documentType,
        parent: selectedAsset,
        changeType: activeDraft.changeType,
        agentId: view === "source" ? "source" : spec.agentId,
        revisionInstruction: activeDraft.changeType === "ai_optimized"
          ? activeDraft.appliedOptimizationInstruction ?? activeDraft.optimizationInstruction
          : activeDraft.optimizationInstruction || undefined,
        metadata,
        expectedHeadAssetId: preservingBranch ? undefined : current?.asset.id,
        allowBranch: preservingBranch,
        provenance: view === "source"
          ? sourceParent ? {
              originalInput: getDocumentMeta(sourceParent)?.text ?? String(sourceParent.params?.text ?? ""),
              generationInput: body,
              sourceMaterials: [snapshotAsset(sourceParent, getDocumentMeta(sourceParent)?.text, "共享原始资料")],
              parentAssetIds: [sourceParent.asset.id],
            } : { originalInput: body, generationInput: body, sourceMaterials: [], parentAssetIds: [] }
          : preservingBranch ? {
              originalInput: selectedMeta?.provenance?.originalInput,
              generationInput: body,
              systemInstruction: selectedMeta?.provenance?.systemInstruction,
              sourceMaterials: preservedMaterials,
              parentAssetIds: preservedParentIds,
            } : {
              originalInput: dependencyContext(view),
              generationInput: body,
              systemInstruction: spec.agentId ? agentSystem(spec.agentId) : undefined,
              sourceMaterials: dependencies.map((asset) => snapshotAsset(asset, getDocumentMeta(asset)?.text, asset.source)),
              parentAssetIds: dependencies.map((asset) => asset.asset.id),
            },
      });
      writeDraft(editorKey, undefined);
      const nextStageKey = `${projectId}:${workflowId}:${view}`;
      const nextEditorKey = `${nextStageKey}:${saved.asset.id}`;
      const clean: StageDraft = {
        text: getDocumentMeta(saved)?.text ?? body,
        optimizationInstruction: activeDraft.changeType === "ai_optimized"
          ? activeDraft.appliedOptimizationInstruction ?? activeDraft.optimizationInstruction
          : activeDraft.optimizationInstruction,
        changeType: "manual",
        sourceKind,
        sourceAssetId: activeDraft.sourceAssetId,
        dependencyMode: preservingBranch ? "view_history" : "current",
        ...savedReview(saved),
      };
      setSelectedByStage((state) => ({ ...state, [nextStageKey]: saved.asset.id }));
      setDrafts((state) => ({ ...state, [nextEditorKey]: clean }));
      setMessage(preservingBranch
        ? `历史分支已保存为 v${getDocumentMeta(saved)?.version ?? 1}，旧依赖保持不变，未替代当前生产版本。`
        : `已保存为 v${getDocumentMeta(saved)?.version ?? 1}。${reviewValid ? "质量审查随版本保存。" : "该版本仍待质量审查，下游不会放行。"}`);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(null);
    }
  };

  const generate = async () => {
    const spec = stageSpec(view);
    if (!spec?.agentId || busy || view === "source" || view === "videos") return;
    if (contentDirty) {
      setError("当前正文有未保存修改。请先保存、放弃草稿，或使用 LLM 优化当前草稿。");
      return;
    }
    if (!dependenciesReady(view)) {
      setError(`无法生成：请先处理 ${blockedDependencies.map((id) => stageSpec(id)?.label).join("、") || "前序依赖"}`);
      return;
    }
    setBusy("generate");
    setError("");
    setMessage("");
    try {
      const output = cleanAndValidateVideoMarkdown(view, await callVideoLlmWithSafetyRetry((system, user) => llmChat(system, user, llmModel), agentSystem(spec.agentId), prepareVideoAgentContext(`${projectContext}\n\n${dependencyContext(view)}`)));
      let review: VideoQualityReviewSummary;
      try {
        review = await reviewOutput(view, output);
      } catch (reviewError) {
        updateDraft({ text: output, changeType: "generated", appliedOptimizationInstruction: undefined, dependencyMode: historical ? "migrate_latest" : "current" }, true);
        setDocumentView(selectedMeta ? "compare" : "markdown");
        setError(`草稿已保留，但独立质量审查失败：${String(reviewError)}`);
        return;
      }
      updateDraft({
        text: output,
        changeType: "generated",
        appliedOptimizationInstruction: undefined,
        dependencyMode: historical ? "migrate_latest" : "current",
        reviewMarkdown: review.markdown,
        reviewStatus: review.status,
        reviewScore: review.score,
        reviewedText: output,
        reviewAt: Date.now(),
        reviewPolicyVersion: VIDEO_REVIEW_POLICY_VERSION,
      });
      setDocumentView(selectedMeta ? "compare" : view === "anchors" || view === "storyboard" ? "overview" : "markdown");
      setMessage(`${agentLabel(spec.agentId)}已生成并完成独立审查：${review.status === "passed" ? "通过" : "需修改"}${review.score === null ? "" : ` · ${review.score}分`}。结果仍是未保存草稿。`);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(null);
    }
  };

  const optimize = async () => {
    const spec = stageSpec(view);
    if (!spec?.agentId || view === "source" || view === "videos" || busy) return;
    const instruction = activeDraft.optimizationInstruction.trim();
    if (!instruction) {
      setError("请先填写 LLM 优化要求");
      return;
    }
    if (!activeDraft.text.trim()) {
      setError("当前没有可优化的 Markdown");
      return;
    }
    if (historical && activeDraft.dependencyMode !== "migrate_latest") {
      setError("联动优化会直接更新当前生产链。请先选择迁移到最新依赖；历史分支不会自动改写后续产物。");
      return;
    }
    if (activeDraft.dependencyMode === "preserve_history") {
      setError("联动优化不写入历史分支。请选择迁移到最新依赖后再执行。");
      return;
    }
    if (!dependenciesReady(view)) {
      setError(`无法优化：请先处理 ${blockedDependencies.map((id) => stageSpec(id)?.label).join("、") || "前序依赖"}`);
      return;
    }
    const start = videoOptimizationCascade.indexOf(view);
    const cascadeStages = videoOptimizationCascade
      .slice(start)
      .filter((stage) => stage === view || Boolean(stageAsset(stage)));
    const dirtyDownstream = cascadeStages.slice(1).find((stage) => stageHasUnsavedDraft(stage));
    if (dirtyDownstream) {
      setError(`${stageSpec(dirtyDownstream)?.label}有未保存草稿。请先处理该草稿，避免联动优化覆盖其保存基线。`);
      return;
    }
    setBusy("optimize");
    setError("");
    setMessage("");
    try {
      const workingTexts: Partial<Record<View, string>> = {};
      const prepared: Array<{ stage: VideoMarkdownStage; text: string; review: VideoQualityReviewSummary }> = [];
      const dependencyContextFor = (stage: VideoMarkdownStage) => dependencyAssets(stage)
        .map((asset) => {
          const dependencyStage = dependencyViews(stage).find((candidate) => stageAsset(candidate)?.asset.id === asset.asset.id);
          const text = dependencyStage ? workingTexts[dependencyStage] ?? getDocumentMeta(asset)?.text ?? "" : getDocumentMeta(asset)?.text ?? "";
          const priorChapter = stage === "anchors" && asset.params?.chapterNo !== chapterNo;
          return `# ${priorChapter ? `上章已确认锚点 · ${chapterLabel(asset)}` : asset.source}\n\n${text}`;
        })
        .join("\n\n---\n\n");
      for (let stageIndex = 0; stageIndex < cascadeStages.length; stageIndex += 1) {
        const stage = cascadeStages[stageIndex];
        const stageSpecValue = stageSpec(stage)!;
        setMessage(`正在联动优化 ${stageSpecValue.label}（${stageIndex + 1}/${cascadeStages.length}）并执行独立审查…`);
        const baseText = stage === view ? activeDraft.text : getDocumentMeta(stageAsset(stage)!)?.text ?? "";
        const stageInstruction = stage === view
          ? instruction
          : `这是由上游“${stageSpec(view)?.label}”优化触发的联动更新。依据本次已经更新的全部上游，直接修订当前${stageSpecValue.label}，消除旧版本冲突；同时落实用户总要求：${instruction}`;
        const effectiveDependencies = dependencyContextFor(stage);
        const request = (markdown: string, extraInstruction = stageInstruction) => prepareVideoAgentContext(buildVideoOptimizationRequest({
          instruction: extraInstruction,
          markdown,
          projectContext,
          dependencyContext: effectiveDependencies,
          workspaceContext: optimizationWorkspaceContext(stage, workingTexts),
        }));
        let output = cleanAndValidateVideoMarkdown(stage, await callVideoLlmWithSafetyRetry(
          (system, user) => llmChat(system, user, llmModel),
          videoOptimizationSystem(stage, agentSystem(stageSpecValue.agentId!)),
          request(baseText),
        ));
        let review = await reviewOutput(stage, output, { dependencyContext: effectiveDependencies, workingTexts });
        if (review.status !== "passed") {
          output = cleanAndValidateVideoMarkdown(stage, await callVideoLlmWithSafetyRetry(
            (system, user) => llmChat(system, user, llmModel),
            videoOptimizationSystem(stage, agentSystem(stageSpecValue.agentId!)),
            request(output, `${stageInstruction}\n\n必须修复以下独立审查阻断后再返回完整当前目标：\n${review.markdown}`),
          ));
          review = await reviewOutput(stage, output, { dependencyContext: effectiveDependencies, workingTexts });
        }
        if (review.status !== "passed") {
          throw new Error(`${stageSpecValue.label}经过联动优化和一次自动修订后仍未通过独立审查，未开始写入任何新版本：${review.blockingIssues || review.markdown}`);
        }
        if (stage === "qc" && !parseVideoQcVerdict(output).passed) {
          throw new Error("联动生成的质检报告结论仍不可生成，未开始写入任何新版本");
        }
        workingTexts[stage] = output;
        prepared.push({ stage, text: output, review });
      }

      const savedByStage: Partial<Record<View, LibAsset>> = {};
      const draftUpdates: Record<string, StageDraft> = {};
      const selectionUpdates: Record<string, string> = {};
      setMessage(`全部联动草稿已通过审查，正在按依赖顺序保存 ${prepared.length} 个新版本…`);
      for (const item of prepared) {
        const stageSpecValue = stageSpec(item.stage)!;
        const parent = item.stage === view ? selectedAsset : stageAsset(item.stage);
        const head = stageAsset(item.stage);
        const replacements = new Map<string, LibAsset>();
        for (const dependencyStage of dependencyViews(item.stage)) {
          const old = stageAsset(dependencyStage);
          const replacement = savedByStage[dependencyStage];
          if (old && replacement) replacements.set(old.asset.id, replacement);
        }
        const dependencies = dependencyAssets(item.stage).map((asset) => replacements.get(asset.asset.id) ?? asset);
        const bindingAsset = (stage: View) => savedByStage[stage] ?? stageAsset(stage);
        const metadata: Record<string, unknown> = {
          ...currentLineage(),
          videoBranch: false,
          dependencyMode: "current",
          agentReview: item.review.markdown,
          agentReviewStatus: item.review.status,
          agentReviewScore: item.review.score,
          agentReviewedText: item.text,
          agentReviewAt: Date.now(),
          agentReviewPolicyVersion: VIDEO_REVIEW_POLICY_VERSION,
          ...(item.stage === "qc" ? qcBindings(bindingAsset, project?.videoModel, project?.aspectRatio, project?.videoResolution || "720p") : {}),
        };
        const titleSuffix = typeof metadata.chapterNo === "number" ? ` · 第${metadata.chapterNo}章` : "";
        const saved = await saveDocumentVersion({
          title: `${stageSpecValue.title}${titleSuffix}`,
          text: item.text,
          model: llmModel,
          projectId,
          documentType: stageSpecValue.documentType!,
          parent,
          changeType: "ai_optimized",
          agentId: stageSpecValue.agentId,
          revisionInstruction: instruction,
          metadata,
          expectedHeadAssetId: head?.asset.id,
          provenance: {
            originalInput: dependencyContextFor(item.stage),
            generationInput: item.text,
            systemInstruction: agentSystem(stageSpecValue.agentId!),
            sourceMaterials: dependencies.map((asset) => snapshotAsset(asset, getDocumentMeta(asset)?.text, asset.source)),
            parentAssetIds: dependencies.map((asset) => asset.asset.id),
          },
        });
        savedByStage[item.stage] = saved;
        const oldEditorKey = `${projectId}:${workflowId}:${item.stage}:${parent?.asset.id ?? "new"}`;
        writeDraft(oldEditorKey, undefined);
        const savedStageKey = `${projectId}:${workflowId}:${item.stage}`;
        const savedEditorKey = `${savedStageKey}:${saved.asset.id}`;
        selectionUpdates[savedStageKey] = saved.asset.id;
        draftUpdates[savedEditorKey] = {
          text: item.text,
          optimizationInstruction: instruction,
          changeType: "manual",
          dependencyMode: "current",
          ...savedReview(saved),
        };
      }
      setSelectedByStage((state) => ({ ...state, ...selectionUpdates }));
      setDrafts((state) => ({ ...state, ...draftUpdates }));
      setDocumentView("compare");
      setMessage(`AI 联动优化完成：${prepared.map((item) => stageSpec(item.stage)?.label).join(" → ")} 已直接保存为新版本并通过独立审查。已有视频资源未自动重生成。`);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(null);
    }
  };

  const reviewCurrent = async () => {
    if (view === "source" || view === "videos" || busy || !activeDraft.text.trim()) return;
    if (historical && (activeDraft.dependencyMode === "view_history" || activeDraft.dependencyMode === "current")) {
      setError("历史版本处于只读查看状态。请先选择分支或迁移策略。");
      return;
    }
    if (activeDraft.dependencyMode !== "preserve_history" && !dependenciesReady(view)) {
      setError(`无法审查：请先处理 ${blockedDependencies.map((id) => stageSpec(id)?.label).join("、") || "前序依赖"}`);
      return;
    }
    setBusy("review");
    setError("");
    setMessage("");
    try {
      const review = await reviewOutput(view, activeDraft.text);
      updateDraft({ reviewMarkdown: review.markdown, reviewStatus: review.status, reviewScore: review.score, reviewedText: activeDraft.text, reviewAt: Date.now(), reviewPolicyVersion: VIDEO_REVIEW_POLICY_VERSION });
      setMessage(`独立质量审查完成：${review.status === "passed" ? "通过" : "需修改"}${review.score === null ? "" : ` · ${review.score}分`}。审查报告会在保存时绑定到该正文版本。`);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(null);
    }
  };

  const useReviewSuggestions = () => {
    if (!activeDraft.reviewMarkdown) return;
    try {
      const review = parseVideoQualityReview(activeDraft.reviewMarkdown);
      updateDraft({ optimizationInstruction: review.suggestions || review.blockingIssues || "根据质量审查报告修复全部阻断问题，并保持未指出的内容不变。" });
      setMessage("已把审查建议带入优化要求，请检查后点击“AI 优化”。");
    } catch (cause) {
      setError(String(cause));
    }
  };

  const setStoryboard = (shots: StoryboardShot[]) => updateDraft({
    text: serializeStoryboard(shots),
    changeType: activeDraft.changeType === "generated" || activeDraft.changeType === "ai_optimized" ? activeDraft.changeType : "manual",
  }, true);

  const onPickSource = (asset: LibAsset) => {
    const nextWorkflowId = videoWorkflowIdForSource(asset);
    const nextStageKey = `${projectId}:${nextWorkflowId}:source`;
    const existing = latest(assets, projectId, "source", nextWorkflowId);
    const selectedId = existing?.asset.id;
    const nextEditorKey = `${nextStageKey}:${selectedId ?? "new"}`;
    const sourceKind: SourceKind = asset.params?.sourceKind === "novel_chapter" || getDocumentMeta(asset)?.documentType === "novel" ? "novel" : "idea";
    const nextDraft: StageDraft = {
      text: getDocumentMeta(asset)?.text ?? String(asset.params?.text ?? ""),
      optimizationInstruction: "",
      changeType: "manual",
      sourceKind,
      sourceAssetId: asset.asset.id,
      dependencyMode: "current",
    };
    setActiveWorkflowId(nextWorkflowId);
    try { localStorage.setItem(`video-md:last-workflow:${projectId}`, nextWorkflowId); } catch { /* in-memory selection remains available */ }
    setView("source");
    setSelectedByStage((state) => ({ ...state, [nextStageKey]: selectedId }));
    setDrafts((state) => ({ ...state, [nextEditorKey]: nextDraft }));
    writeDraft(nextEditorKey, nextDraft);
    setPickerOpen(false);
    setMessage(existing
      ? "已打开该章节的视频工作区，并载入共享章节的最新正文作为待保存草稿"
      : "已建立该章节的视频工作区；请检查正文后保存原始资料版本");
    setError("");
  };

  const stageStatus = (id: View): VideoStageStatus => {
    if (id === "videos") {
      return videoResultStageStatus(savedStoryboardShots.length, completedShotCount, failedVideoTasks.length, storyboardReady);
    }
    if (stageHasUnsavedDraft(id)) return "draft";
    const asset = stageAsset(id);
    if (!asset) return dependencyViews(id).some((dependency) => !stageUsable(dependency)) ? "blocked" : "not_started";
    if (stale(id)) return "stale";
    if (id !== "source") {
      const status = productionReviewStatus(asset);
      if (!status) return "review_required";
      if (status === "needs_changes") return "needs_changes";
    }
    return "current";
  };
  const nextView = views[Math.min(views.findIndex((item) => item.id === view) + 1, views.length - 1)]?.id;
  const recommendation = blockedDependencies.length
    ? `先处理：${blockedDependencies.map((id) => stageSpec(id)?.label).join("、")}`
    : view === "source" && sourceOutdated
      ? "小说章节已有新修订，先载入最新版并确认迁移"
    : !activeDraft.text.trim()
      ? view === "source" ? "先选择小说章节，或直接输入脑洞" : `生成${stageSpec(view)?.label}草稿`
      : !reviewValid && view !== "source"
        ? "先审查当前草稿，确认忠实度、剧情质量和可执行性"
        : activeReviewStatus === "needs_changes"
          ? "按审查报告修复阻断问题"
          : dirty
            ? "人工检查后保存当前草稿"
            : nextView && nextView !== view ? `当前阶段已就绪，可进入${stageSpec(nextView)?.label}` : "当前工作流已完成";
  const saveLabel = activeDraft.dependencyMode === "preserve_history"
    ? "保存历史分支"
    : activeDraft.dependencyMode === "migrate_latest"
      ? "保存迁移版本"
      : selectedAsset ? `保存 v${Math.max(...versions.map((asset) => getDocumentMeta(asset)?.version ?? 0), 0) + 1}` : "保存初稿";
  const viewingHistoricalVersion = historical && activeDraft.dependencyMode === "view_history";
  const supersededWhileEditing = historical && activeDraft.dependencyMode === "current";
  const needsDependencyChoice = viewingHistoricalVersion || supersededWhileEditing;
  const canEdit = !viewingHistoricalVersion;
  const canSave = Boolean(activeDraft.text.trim() && dirty && !busy && canEdit && !needsDependencyChoice);
  const generateDisabledReason = busy ? "当前已有操作进行中" : !canEdit ? "历史版本只读，请先选择分支或迁移策略" : contentDirty ? "当前正文有未保存修改" : !dependenciesReady(view) ? `前序阶段未就绪：${blockedDependencies.map((id) => stageSpec(id)?.label).join("、")}` : "";
  const cascadeStart = videoOptimizationCascade.indexOf(view as VideoMarkdownStage);
  const visibleCascadeStages = cascadeStart < 0 ? [] : videoOptimizationCascade.slice(cascadeStart).filter((stage) => stage === view || Boolean(stageAsset(stage)));
  const dirtyCascadeStage = visibleCascadeStages.slice(1).find((stage) => stageHasUnsavedDraft(stage));
  const optimizeDisabledReason = busy ? "当前已有操作进行中"
    : !canEdit ? "历史版本只读，请先选择迁移策略"
      : activeDraft.dependencyMode === "preserve_history" ? "联动优化不写入历史分支，请迁移到最新依赖"
        : historical && activeDraft.dependencyMode !== "migrate_latest" ? "历史版本必须先迁移到最新依赖"
          : !activeDraft.text.trim() ? "当前没有可优化的正文"
            : !activeDraft.optimizationInstruction.trim() ? "请先填写优化要求"
              : !dependenciesReady(view) ? `前序阶段未就绪：${blockedDependencies.map((id) => stageSpec(id)?.label).join("、")}`
                : dirtyCascadeStage ? `${stageSpec(dirtyCascadeStage)?.label}有未保存草稿，请先处理`
                  : "";
  const reviewDisabledReason = busy ? "当前已有操作进行中" : !canEdit ? "历史版本只读，请先选择分支或迁移策略" : !activeDraft.text.trim() ? "当前没有可审查的正文" : activeDraft.dependencyMode !== "preserve_history" && !dependenciesReady(view) ? `前序阶段未就绪：${blockedDependencies.map((id) => stageSpec(id)?.label).join("、")}` : "";
  const saveDisabledReason = canSave ? "" : needsDependencyChoice ? "必须先选择历史分支或迁移策略" : !dirty ? "当前没有需要保存的正文或审查变更" : !activeDraft.text.trim() ? "文档内容为空" : busy ? "当前已有操作进行中" : "当前状态不能保存";
  const stageNavigationItems = views.map((item) => ({
    id: item.id,
    label: item.label,
    status: stageStatus(item.id),
    detail: item.id === "videos" && savedStoryboardShots.length > 0
      ? `${completedShotCount}/${savedStoryboardShots.length} 镜已生成${failedVideoTasks.length ? ` · ${failedVideoTasks.length} 个失败任务` : ""}`
      : undefined,
  }));

  return <section aria-label="Markdown 短剧视频工作台" className="flex min-h-0 flex-1 flex-col overflow-hidden" onKeyDown={(event) => {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      if (canSave) void save();
    }
  }}>
    <header className="border-b border-slate-800 bg-slate-950/40 px-5 py-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div><p className="text-sm text-slate-200">短剧视频 Markdown 工作台</p><p className="mt-1 text-[11px] text-slate-500">章节文字可共享；图片资产、视频资产和各自分镜继续隔离。</p></div>
        <div className="flex flex-wrap items-center gap-2">
          {workspaceSources.length > 0 && <label className="flex items-center gap-2 text-xs text-slate-400">工作区<select aria-label="视频工作区" value={requiresWorkspaceSelection ? "" : workflowId} onChange={(event) => { if (event.target.value) chooseWorkflow(event.target.value); }} className="max-w-64 rounded-lg border border-slate-700 bg-slate-950 px-2 py-2 text-xs text-slate-200">{requiresWorkspaceSelection && <option value="">请选择工作区</option>}{workspaceSources.map((asset) => <option key={workflowIdFromSource(asset)} value={workflowIdFromSource(asset)}>{chapterLabel(asset)} · {asset.source}</option>)}</select></label>}
          <button className={button} onClick={() => setPickerOpen(true)}><FileInput size={13} className="mr-1 inline" />切换章节 / 资料</button>
        </div>
      </div>
      {!requiresWorkspaceSelection && <VideoStageNavigation idPrefix={stageTabsId} items={stageNavigationItems} current={view} onSelect={(id) => { setView(id); setMessage(""); setError(""); }} />}
    </header>

    {!requiresWorkspaceSelection && views.filter((item) => item.id !== view).map((item) => <div
      key={item.id}
      id={`${stageTabsId}-panel-${item.id}`}
      role="tabpanel"
      aria-labelledby={`${stageTabsId}-tab-${item.id}`}
      hidden
    />)}
    <div
      id={requiresWorkspaceSelection ? undefined : `${stageTabsId}-panel-${view}`}
      role={requiresWorkspaceSelection ? undefined : "tabpanel"}
      aria-labelledby={requiresWorkspaceSelection ? undefined : `${stageTabsId}-tab-${view}`}
      className="min-h-0 flex-1 overflow-y-auto p-5"
    >
      {requiresWorkspaceSelection ? <section className="mx-auto max-w-3xl rounded-2xl border border-cyan-300/15 bg-slate-950/40 p-6"><h2 className="text-lg font-semibold text-slate-100">请选择视频工作区</h2><p className="mt-2 text-sm leading-6 text-slate-400">当前项目有多个小说章节或脑洞工作区。为避免误改最近保存的其他章节，系统不会替你自动选择。</p><div className="mt-5 grid gap-3 sm:grid-cols-2">{workspaceSources.map((asset) => <button key={workflowIdFromSource(asset)} onClick={() => chooseWorkflow(workflowIdFromSource(asset)!)} className="rounded-xl border border-slate-700 bg-slate-900/50 p-4 text-left hover:border-cyan-300/30"><span className="block text-sm font-medium text-slate-100">{chapterLabel(asset)} · {asset.source}</span><span className="mt-2 block text-xs text-slate-500">原始资料 v{getDocumentMeta(asset)?.version ?? 1}</span></button>)}</div><button className={`${button} mt-4`} onClick={() => setPickerOpen(true)}>选择其他小说章节 / 资料</button></section> : view === "videos" ? <div className="space-y-4">
        <div className="flex flex-wrap items-start justify-between gap-3 rounded-xl border border-slate-800 bg-slate-950/30 p-4"><div><h2 className="text-base font-semibold">逐镜视频资源 · {completedShotCount}/{savedStoryboardShots.length || 0}</h2><p className="mt-1 text-xs text-slate-500">一个分镜对应一次请求和独立资源；这里不自动拼接。{failedVideoTasks.length ? ` 当前分镜有 ${failedVideoTasks.length} 个失败任务。` : ""}</p></div><button disabled={!storyboardReady} className={primary} onClick={() => { if (storyboardAsset) onSendToVideo(parseStoryboardShots(getDocumentMeta(storyboardAsset)?.text ?? ""), storyboardAsset, { anchorAssetId: anchorAsset?.asset.id, qcAssetId: qcAsset?.asset.id, approvedModel: project?.videoModel ?? "", approvedAspectRatio: project?.aspectRatio ?? "", approvedResolution: project?.videoResolution || "720p" }); }}><Clapperboard size={13} className="mr-1 inline" />发送当前分镜到视频生成</button></div>
        {!storyboardReady && <section className="rounded-xl border border-rose-300/15 bg-rose-300/5 p-4"><h3 className="text-sm font-semibold text-rose-100">当前不能进入视频生成</h3><ul className="mt-2 space-y-1 text-xs text-rose-200/80">{gateReasons.map((reason) => <li key={reason}>• {reason}</li>)}</ul></section>}
        <div className="space-y-3">{parseStoryboardShots(storyboardAsset ? getDocumentMeta(storyboardAsset)?.text ?? "" : "").map((shot) => { const resources = videos.filter((asset) => Number(asset.params?.shotNo) === shot.shotNo); return <article key={shot.shotNo} className="rounded-xl border border-slate-800 bg-slate-950/30 p-4"><div className="flex flex-wrap items-center gap-3"><span className="rounded-full bg-violet-400/10 px-2 py-1 text-xs text-violet-100">第 {shot.shotNo} 镜 · {shot.durationS} 秒</span><span className="min-w-0 flex-1 truncate text-xs text-slate-400">{shot.scene} · {shot.shotType}</span><span className="text-xs text-slate-500">{resources.length ? `${resources.length} 个资源版本` : "未生成"}</span></div>{resources.length > 0 && <div className="mt-3 grid gap-2 md:grid-cols-3">{resources.map((asset) => <div key={asset.asset.id} className="rounded-lg border border-white/5 p-3"><div className="truncate text-xs text-slate-200">{asset.source}</div><div className="mt-1 text-[10px] text-slate-500">{asset.model} · {asset.asset.durationS ?? shot.durationS} 秒</div></div>)}</div>}</article>; })}</div>
      </div> : <div className="grid items-start gap-5 lg:grid-cols-[minmax(0,1fr)_340px] xl:grid-cols-[minmax(0,1fr)_380px]">
        <main className="min-w-0">
          <div className="sticky top-0 z-10 mb-3 rounded-xl border border-slate-800 bg-slate-950/95 p-3 shadow-xl shadow-black/10 backdrop-blur">
            <div className="flex flex-wrap items-start justify-between gap-3"><div><h2 className="text-base font-semibold">{stageSpec(view)?.label}</h2><p className="mt-1 text-xs text-slate-500">{selectedAsset ? `v${selectedMeta?.version ?? 1} · ${documentChangeLabel(selectedMeta?.changeType ?? "manual")}` : "尚未保存"}{historical ? " · 历史版本" : ""}{dirty ? " · 草稿未保存" : ""}{view === "source" && sourceParent ? ` · 来源：${sourceParent.source}` : ""}</p></div><div className="flex flex-wrap gap-2">{dirty && <button className={button} disabled={Boolean(busy)} onClick={(event) => openDiscardDialog(event.currentTarget)}><RotateCcw size={13} className="mr-1 inline" />放弃草稿并恢复已保存内容…</button>}<button title={saveDisabledReason || undefined} className={primary} disabled={!canSave} onClick={() => void save()}>{busy === "save" ? <Loader2 size={13} className="mr-1 inline animate-spin" /> : <Save size={13} className="mr-1 inline" />}{saveLabel}</button></div></div>
            <div className="mt-3 flex flex-wrap items-center gap-2 border-t border-white/5 pt-3">{versions.length > 0 && <label className="flex items-center gap-2 text-xs text-slate-400"><History size={13} />保存版本<select aria-label="保存版本" value={selectedAssetId ?? ""} onChange={(event) => selectVersion(event.target.value)} className="rounded-lg border border-slate-700 bg-slate-950 px-2 py-2 text-xs text-slate-200">{versions.map((asset) => { const meta = getDocumentMeta(asset); const branch = asset.params?.videoBranch === true; return <option key={asset.asset.id} value={asset.asset.id}>v{meta?.version ?? 1}{asset.asset.id === current?.asset.id ? "（当前生产版）" : branch ? "（历史分支）" : "（历史）"} · {documentChangeLabel(meta?.changeType ?? "manual")}</option>; })}</select></label>}{view === "source" && <select value={activeDraft.sourceKind ?? "novel"} onChange={(event) => updateDraft({ sourceKind: event.target.value as SourceKind, changeType: "manual" })} className="rounded-lg border border-slate-700 bg-slate-950 px-2 py-2 text-xs text-slate-200"><option value="novel">小说 / 章节</option><option value="idea">脑洞 / 梗概</option></select>}<span className="ml-auto text-[10px] text-slate-500">Ctrl/Cmd + S 保存可用草稿</span></div>
            {needsDependencyChoice && <div role="alert" className="mt-3 flex flex-wrap items-center gap-2 rounded-lg border border-amber-300/15 bg-amber-300/5 p-3 text-xs text-amber-100"><GitBranch size={14} /><span className="mr-auto">{supersededWhileEditing ? "编辑期间出现了新的当前生产版。当前草稿仍完整保留，但必须明确选择依赖策略后才能保存或继续调用 AI。" : "正在只读查看历史版本。不会自动把旧内容伪装成已适配最新依赖。"}</span><button className={button} onClick={() => beginHistoricalDraft("preserve_history")}>按原依赖创建分支</button><button className={button} onClick={() => beginHistoricalDraft("migrate_latest")}>迁移到最新依赖…</button></div>}
          </div>
          <section className="mb-3 flex flex-wrap items-center gap-3 rounded-xl border border-cyan-300/15 bg-cyan-300/5 p-3"><ArrowRight size={15} className="text-cyan-200" /><div><div className="text-[10px] uppercase tracking-wider text-cyan-300/70">推荐下一步</div><div className="mt-1 text-sm text-cyan-50">{recommendation}</div></div>{blockedDependencies.length > 0 && <div className="ml-auto flex flex-wrap gap-2">{blockedDependencies.map((id) => <button key={id} className={button} onClick={() => setView(id)}>打开{stageSpec(id)?.label}</button>)}</div>}</section>
          {view === "source" && sourceOutdated && <section role="alert" className="mb-3 flex flex-wrap items-center gap-3 rounded-xl border border-amber-300/20 bg-amber-300/5 p-3 text-xs text-amber-100"><span className="mr-auto">当前视频工作区仍基于旧章节修订；系统不会自动覆盖。载入最新版后请审阅并保存新的原始资料版本。</span><button className={button} onClick={loadLatestChapterRevision}>载入章节最新版</button></section>}
          {!canEdit && <div className="mb-3 rounded-xl border border-amber-300/15 bg-amber-300/5 p-3 text-xs text-amber-100">历史版本当前为只读。选择分支或迁移策略后才能编辑。</div>}
          <VideoDocumentViews view={currentDocumentView} onViewChange={setDocumentView} stage={view} stageLabel={stageSpec(view)?.label ?? view} text={activeDraft.text} baseline={selectedMeta?.text ?? ""} editable={canEdit} storyboardShots={storyboardShots} storyboardRoundTripSafe={isStoryboardRoundTripSafe(activeDraft.text)} onTextChange={(text) => updateDraft({ text, changeType: activeDraft.changeType === "generated" || activeDraft.changeType === "ai_optimized" ? activeDraft.changeType : "manual" }, true)} onStoryboardChange={setStoryboard} />
          {message && <p role="status" className="mt-3 rounded-lg border border-cyan-300/10 bg-cyan-300/5 p-3 text-xs text-cyan-100">{message}</p>}
          {error && <p role="alert" className="mt-3 rounded-lg border border-rose-300/15 bg-rose-300/5 p-3 text-xs text-rose-200">{error}</p>}
        </main>

        <aside className="space-y-4 lg:sticky lg:top-0">
          {view !== "source" && <section className="rounded-xl border border-cyan-300/15 bg-slate-950/45 p-4"><div className="flex items-center justify-between"><div><h3 className="text-sm font-semibold text-cyan-50">AI 创作助手</h3><p className="mt-1 text-[11px] text-slate-500">生成只进入草稿；AI 优化会直接保存当前及已有下游文字产物。</p></div><Sparkles size={16} className="text-cyan-300" /></div><div className="mt-3 grid grid-cols-2 gap-2"><button title={generateDisabledReason || undefined} className={button} disabled={Boolean(generateDisabledReason)} onClick={() => void generate()}>{busy === "generate" ? <Loader2 size={13} className="mr-1 inline animate-spin" /> : null}AI 生成草稿</button><button title={optimizeDisabledReason || undefined} className={button} disabled={Boolean(optimizeDisabledReason)} onClick={() => void optimize()}>{busy === "optimize" ? <Loader2 size={13} className="mr-1 inline animate-spin" /> : null}AI 优化</button></div>{(generateDisabledReason || optimizeDisabledReason) && <div className="mt-2 space-y-1 text-[10px] leading-4 text-amber-200/80">{generateDisabledReason && <div>生成不可用：{generateDisabledReason}</div>}{optimizeDisabledReason && <div>优化不可用：{optimizeDisabledReason}</div>}</div>}<label className="mt-3 block text-xs text-slate-300">优化要求<textarea aria-label="LLM 优化要求" value={activeDraft.optimizationInstruction} onChange={(event) => updateDraft({ optimizationInstruction: event.target.value })} placeholder={view === "anchors" ? "例如：继承固定外貌，只把本章伤势和换装设为剧情锚点" : "例如：删掉越界续写，增强人物选择与因果，未提及内容保持不变"} className={`${field} mt-1 min-h-24 resize-y`} /></label><p className="mt-2 text-[10px] leading-4 text-slate-500">将按当前阶段 → 后续已有阶段逐个优化和独立审查，全部通过后直接保存新版本。可能调用多次文本模型；图片和视频不会自动重生成。</p><details className="mt-3 border-t border-white/5 pt-3"><summary className="cursor-pointer text-[11px] text-slate-500">本阶段 Agent 设置</summary><button className={`${button} mt-2 w-full`} onClick={() => onEditPrompt(stageSpec(view)?.agentId ?? "")}>查看 / 修改 Agent Prompt</button></details></section>}

          {view !== "source" && <><VideoQualityReviewCard review={reviewValid ? activeDraft.reviewMarkdown : undefined} status={activeReviewStatus} score={reviewValid ? activeDraft.reviewScore : undefined} busy={busy === "review"} disabled={Boolean(reviewDisabledReason)} onReview={() => void reviewCurrent()} onUseSuggestions={useReviewSuggestions} />{reviewDisabledReason && <p className="-mt-2 text-[10px] text-amber-200/80">审查不可用：{reviewDisabledReason}</p>}</>}

          <section className="rounded-xl border border-slate-800 bg-slate-950/30 p-4"><div className="text-xs font-semibold text-slate-300">依赖检查</div><div className="mt-3 space-y-2">{dependencyViews(view).map((id) => { const latestAsset = stageAsset(id); const parentIds = new Set(selectedMeta?.provenance?.parentAssetIds ?? []); const referenced = assets.find((asset) => parentIds.has(asset.asset.id) && isStageAsset(asset, id)); const state = !latestAsset ? "缺失" : !stageUsable(id) ? stale(id) ? "需更新" : productionReviewStatus(latestAsset) === "needs_changes" ? "质量未通过" : "待审查" : "当前"; const hasDraft = stageHasUnsavedDraft(id); return <div key={id} className="rounded-lg border border-white/5 p-3 text-[11px]"><div className="flex items-center gap-2"><span className={state === "当前" ? "text-emerald-300" : "text-amber-300"}>{state === "当前" ? "✓" : "○"}</span><span className="font-medium text-slate-300">{stageSpec(id)?.label}</span><span className="ml-auto text-slate-500">{state}</span></div><div className="mt-2 text-slate-500">当前文档引用：{referenced ? `v${getDocumentMeta(referenced)?.version ?? 1}` : "无"} · 最新：{latestAsset ? `v${getDocumentMeta(latestAsset)?.version ?? 1}` : "无"}</div>{hasDraft && <div className="mt-2 rounded bg-cyan-300/5 px-2 py-1 text-cyan-200">另有未保存草稿，不参与当前下游生产</div>}<button className="mt-2 text-[11px] text-cyan-300 hover:text-cyan-100" onClick={() => setView(id)}>打开{stageSpec(id)?.label}</button></div>; })}{view === "source" && <p className="text-[11px] leading-5 text-slate-500">直接引用小说章节或输入脑洞，不需要上传 TXT/MD。切换工作区时，原工作区未保存草稿仍保留。</p>}{view !== "source" && <p className="text-[11px] leading-5 text-slate-500">AI 联动优化会读取本工作区全部文字产物及媒体关联元数据，并直接更新当前阶段和已有下游文字产物；媒体只保留历史结果，不会自动付费重生成。</p>}</div></section>

          {view === "anchors" && <section className="rounded-xl border border-violet-400/15 bg-violet-400/5 p-4"><div className="text-xs font-semibold text-violet-100">跨章锚点承接</div><p className="mt-2 text-[11px] leading-5 text-slate-400">固定锚定跨章继承；伤势、换装、天气、道具归属和场景状态按剧情建立版本锚点。本章保存完整快照。</p><div className="mt-3 space-y-2">{inheritedAnchors.length ? inheritedAnchors.map((asset) => <div key={asset.asset.id} className="rounded-lg border border-white/5 p-2 text-[11px] text-slate-300">{chapterLabel(asset)} · v{getDocumentMeta(asset)?.version ?? 1} · {asset.source}</div>) : <p className="text-[11px] text-slate-500">本章没有可继承的前章锚点。</p>}</div></section>}
        </aside>
      </div>}
    </div>

    <AssetPicker open={pickerOpen} onClose={() => setPickerOpen(false)} kinds={["text"]} strictProject filter={isVideoSourceCandidate} title="选择小说章节或视频原始资料" onPick={onPickSource} />

    {discardConfirmOpen && <div className="fixed inset-0 z-[70] flex items-center justify-center bg-slate-950/80 p-4 backdrop-blur-sm"><section ref={discardDialogRef} role="dialog" aria-modal="true" aria-labelledby="discard-video-draft-title" aria-describedby="discard-video-draft-description" tabIndex={-1} onKeyDown={(event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        closeDiscardDialog();
        return;
      }
      if (event.key !== "Tab") return;
      const dialog = discardDialogRef.current;
      if (!dialog) return;
      const targets = dialogFocusTargets(dialog);
      if (targets.length === 0) {
        event.preventDefault();
        dialog.focus();
        return;
      }
      const currentIndex = targets.indexOf(document.activeElement as HTMLElement);
      if (event.shiftKey && (currentIndex <= 0)) {
        event.preventDefault();
        targets[targets.length - 1].focus();
      } else if (!event.shiftKey && currentIndex === targets.length - 1) {
        event.preventDefault();
        targets[0].focus();
      }
    }} className="w-full max-w-md rounded-2xl border border-amber-300/20 bg-slate-950 p-5 shadow-2xl"><div className="flex items-start gap-3"><span className="rounded-full bg-amber-300/10 p-2 text-amber-200"><AlertTriangle size={18} /></span><div><h2 id="discard-video-draft-title" className="text-base font-semibold text-slate-100">放弃当前草稿？</h2><p id="discard-video-draft-description" className="mt-2 text-sm leading-6 text-slate-400">将恢复“{stageSpec(view)?.label} {selectedMeta ? `v${selectedMeta.version}` : "未保存初始状态"}”。当前正文、AI 优化结果和未保存审查报告会被清除；其他阶段与其他章节草稿不受影响。</p></div></div><div className="mt-5 flex justify-end gap-2"><button ref={discardCancelRef} className={button} onClick={closeDiscardDialog}>继续编辑</button><button className="rounded-lg border border-rose-300/20 bg-rose-500/15 px-3 py-2 text-xs text-rose-100 hover:bg-rose-500/25" onClick={confirmDiscard}>放弃草稿</button></div></section></div>}
  </section>;
}
