import type { LibAsset } from "../../store/useLibraryStore";
import type { MdScope, MdStyleReference } from "./markdownApi";

export const MAX_COMIC_STYLE_REFERENCES = 8;

/** Selection belongs to a novel work, never to a single chapter. */
export function comicStyleReferenceKey(scope: Pick<MdScope, "projectId" | "novelWorkId">): string {
  return `comic-md:style-references:${scope.projectId}:${scope.novelWorkId}`;
}

function optionalString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function declaredNovelWorkId(asset: LibAsset): string | undefined {
  const params = asset.params ?? {};
  const direct = optionalString(params.novelWorkId) ?? optionalString(params.comicWorkId);
  if (direct) return direct;
  const comic = params.comicGeneration;
  if (!comic || typeof comic !== "object") return undefined;
  const record = comic as Record<string, unknown>;
  return optionalString(record.novelWorkId) ?? optionalString(record.comicWorkId);
}

/** Generic project images are allowed; explicitly foreign novel-work images are not. */
export function isComicStyleReferenceCandidate(asset: LibAsset, scope: Pick<MdScope, "projectId" | "novelWorkId">): boolean {
  if (asset.asset.kind !== "image" || asset.projectId !== scope.projectId) return false;
  const owner = declaredNovelWorkId(asset);
  return !owner || owner === scope.novelWorkId;
}

export function createComicStyleReference(asset: LibAsset): MdStyleReference {
  return {
    assetId: asset.asset.id,
    path: asset.asset.path,
    label: asset.source || asset.asset.path.split(/[\\/]/).pop() || "图片参考",
  };
}

export function normalizeComicStyleReferences(value: MdStyleReference[]): MdStyleReference[] {
  const seen = new Set<string>();
  const seenPaths = new Set<string>();
  return value
    .filter((item) => item && item.assetId.trim() && item.path.trim())
    .filter((item) => {
      const path = item.path.trim().toLocaleLowerCase();
      if (seen.has(item.assetId) || seenPaths.has(path)) return false;
      seen.add(item.assetId);
      seenPaths.add(path);
      return true;
    })
    .slice(0, MAX_COMIC_STYLE_REFERENCES)
    .map((item) => ({ ...item, assetId: item.assetId.trim(), path: item.path.trim(), label: item.label.trim() || "图片参考", description: item.description?.trim() || undefined }));
}
