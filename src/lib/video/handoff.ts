import type { LibAsset } from "../../store/useLibraryStore";
import type { VideoParams } from "../../store/useVideoStore";
import type { StoryboardShot } from "./storyboard";
import { storyboardShotsToGenerationItems } from "./storyboard";

export interface VideoHandoffApproval {
  anchorAssetId?: string;
  qcAssetId?: string;
  approvedModel: string;
  approvedAspectRatio: string;
  approvedResolution: string;
}

export function buildReviewedVideoHandoff(
  shots: StoryboardShot[],
  assets: LibAsset[],
  source: LibAsset,
  approval: VideoHandoffApproval,
): Partial<VideoParams> {
  const generationItems = storyboardShotsToGenerationItems(shots).map((item) => {
    const references = item.referenceAssetIds.map((id) => assets.find((asset) => asset.asset.id === id)).filter((asset): asset is LibAsset => Boolean(asset));
    return {
      ...item,
      id: `shot-${item.shotNo}`,
      referenceImages: references.filter((asset) => asset.asset.kind === "image").map((asset) => asset.asset.path),
      referenceVideos: references.filter((asset) => asset.asset.kind === "video").map((asset) => asset.asset.path),
    };
  });
  return {
    shots: generationItems,
    model: approval.approvedModel,
    aspectRatio: approval.approvedAspectRatio,
    resolution: approval.approvedResolution,
    mode: "text",
    images: [],
    videos: [],
    audios: [],
    storyboardSourceAssetId: source.asset.id,
    productionManifest: {
      storyboardAssetId: source.asset.id,
      anchorAssetId: approval.anchorAssetId,
      qcAssetId: approval.qcAssetId,
      approvedModel: approval.approvedModel,
      approvedAspectRatio: approval.approvedAspectRatio,
      approvedResolution: approval.approvedResolution,
      approvedAt: Date.now(),
      shots: generationItems.map((shot) => ({ shotNo: shot.shotNo, prompt: shot.prompt, durationS: shot.durationS, referenceStrategy: shot.referenceStrategy, referenceAssetIds: shot.referenceAssetIds, referenceImages: shot.referenceImages, referenceVideos: shot.referenceVideos })),
    },
  };
}
