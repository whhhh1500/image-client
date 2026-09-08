import type { LibAsset } from "../../store/useLibraryStore";
import type { VideoParams } from "../../store/useVideoStore";
import type { StoryboardShot } from "./storyboard";
import { storyboardShotsToGenerationItems } from "./storyboard";
import { isPublicHttpsUrl } from "./referenceUrl";

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
    const references = item.referenceAssetIds
      .map((id) => assets.find((asset) => asset.asset.id === id && asset.projectId === source.projectId))
      .filter((asset): asset is LibAsset => Boolean(asset));
    const images = references.filter((asset) => asset.asset.kind === "image");
    const videos = references.filter((asset) => asset.asset.kind === "video");
    return {
      ...item,
      id: `shot-${item.shotNo}`,
      referenceAssetIds: item.referenceAssetIds,
      referenceImages: images.filter((asset) => isPublicHttpsUrl(asset.asset.path)).map((asset) => asset.asset.path),
      referenceLocalImages: images.filter((asset) => !isPublicHttpsUrl(asset.asset.path)).map((asset) => ({ assetId: asset.asset.id, path: asset.asset.path, label: asset.source })),
      referenceVideos: videos.filter((asset) => isPublicHttpsUrl(asset.asset.path)).map((asset) => asset.asset.path),
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
      shots: generationItems.map((shot) => ({ shotNo: shot.shotNo, prompt: shot.prompt, durationS: shot.durationS, referenceStrategy: shot.referenceStrategy, referenceAssetIds: shot.referenceAssetIds, referenceImages: shot.referenceImages, referenceVideos: shot.referenceVideos, referenceLocalImages: shot.referenceLocalImages })),
    },
  };
}
