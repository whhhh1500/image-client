import { describe, expect, it } from "vitest";
import type { LibAsset } from "../../store/useLibraryStore";
import { comicStyleReferenceKey, createComicStyleReference, isComicStyleReferenceCandidate, normalizeComicStyleReferences } from "./styleReferences";

const scope = { projectId: "p", novelWorkId: "work-a" };
const asset = (id: string, kind: "image" | "video", params: Record<string, unknown> = {}): LibAsset => ({ asset: { id, kind, path: `D:/${id}.png` }, source: id, projectId: "p", params, createdAt: 1 });

describe("comic work style references", () => {
  it("stores selections at work scope, not chapter scope", () => {
    expect(comicStyleReferenceKey(scope)).toBe("comic-md:style-references:p:work-a");
  });

  it("keeps video and foreign-project/work images outside the candidate set", () => {
    expect(isComicStyleReferenceCandidate(asset("image", "image"), scope)).toBe(true);
    expect(isComicStyleReferenceCandidate(asset("video", "video"), scope)).toBe(false);
    expect(isComicStyleReferenceCandidate({ ...asset("foreign-project", "image"), projectId: "other" }, scope)).toBe(false);
    expect(isComicStyleReferenceCandidate(asset("other-work", "image", { novelWorkId: "work-b" }), scope)).toBe(false);
    expect(isComicStyleReferenceCandidate(asset("same-work", "image", { novelWorkId: "work-a" }), scope)).toBe(true);
  });

  it("deduplicates selections and preserves optional creator notes", () => {
    const first = { ...createComicStyleReference(asset("ink", "image")), description: "黑白水墨，留白和干笔线条" };
    const samePath = { ...createComicStyleReference(asset("same-path", "image")), path: first.path };
    const refs = normalizeComicStyleReferences([first, first, samePath, { ...createComicStyleReference(asset("color", "image")), description: "" }]);
    expect(refs).toHaveLength(2);
    expect(refs[0].description).toBe("黑白水墨，留白和干笔线条");
    expect(refs[1].description).toBeUndefined();
  });
});
