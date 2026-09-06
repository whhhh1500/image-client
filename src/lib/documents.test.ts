import { describe, expect, it } from "vitest";
import { getDocumentDisplayVersion, getDocumentMeta, getDocumentVersions, inferDocumentType, serializeStoryboard } from "./documents";
import type { LibAsset } from "../store/useLibraryStore";

describe("document model", () => {
  it("infers legacy document types", () => {
    expect(inferDocumentType("分镜·副本")).toBe("storyboard");
    expect(inferDocumentType("编剧")).toBe("script");
    expect(inferDocumentType("角色恒定锚")).toBe("anchor");
    expect(inferDocumentType("完整流水线")).toBe("pipeline");
  });

  it("normalizes legacy text assets into versioned documents", () => {
    const asset: LibAsset = {
      asset: { id: "a1", kind: "text", path: "/tmp/a.md" },
      source: "分镜",
      params: { text: '{"shots":[{"shotNo":1,"action":"走入房间"}]}' },
      createdAt: 1,
    };
    const meta = getDocumentMeta(asset)!;
    expect(meta.documentType).toBe("storyboard");
    expect(meta.version).toBe(1);
    expect(meta.shots).toHaveLength(1);
  });

  it("serializes editable storyboard content", () => {
    expect(JSON.parse(serializeStoryboard([{ shotNo: 1, prompt: "夜景" }])).shots[0].prompt).toBe("夜景");
  });

  it("groups legacy copies into one chronological history", () => {
    const original: LibAsset = { asset: { id: "a1", kind: "text", path: "/a.md" }, source: "分镜", projectId: "p1", params: { text: "one" }, createdAt: 1 };
    const copy: LibAsset = { asset: { id: "a2", kind: "text", path: "/b.md" }, source: "分镜 · 副本", projectId: "p1", params: { text: "two" }, createdAt: 2 };
    expect(getDocumentVersions(copy, [copy, original])).toHaveLength(2);
    expect(getDocumentDisplayVersion(original, [copy, original])).toBe(1);
    expect(getDocumentDisplayVersion(copy, [copy, original])).toBe(2);
  });

  it("reads persisted original-material provenance from a document", () => {
    const asset: LibAsset = {
      asset: { id: "doc-1", kind: "text", path: "/doc.md" },
      source: "剧本",
      params: {
        text: "生成结果",
        provenance: {
          schemaVersion: 1,
          originalInput: "原始梗概",
          sourceMaterials: [{ kind: "text", label: "原始资料", text: "资料正文" }],
          parentAssetIds: [],
          recordedAt: 1,
        },
      },
      createdAt: 1,
    };
    expect(getDocumentMeta(asset)?.provenance?.originalInput).toBe("原始梗概");
    expect(getDocumentMeta(asset)?.provenance?.sourceMaterials[0].text).toBe("资料正文");
  });
});
