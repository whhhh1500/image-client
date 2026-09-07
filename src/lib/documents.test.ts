import { beforeEach, describe, expect, it, vi } from "vitest";
import { saveDocumentVersionAtomic } from "./ipc";
import { getDocumentDisplayVersion, getDocumentMeta, getDocumentVersions, inferDocumentType, saveDocumentVersion } from "./documents";
import type { LibAsset } from "../store/useLibraryStore";
import { useLibraryStore } from "../store/useLibraryStore";

vi.mock("./ipc", () => ({ saveDocumentVersionAtomic: vi.fn() }));

beforeEach(() => {
  vi.clearAllMocks();
  useLibraryStore.getState().loadAssets([]);
});

describe("document model", () => {
  it("infers document types", () => {
    expect(inferDocumentType("分镜·副本")).toBe("storyboard");
    expect(inferDocumentType("编剧")).toBe("script");
    expect(inferDocumentType("角色恒定锚")).toBe("anchor");
    expect(inferDocumentType("完整流水线")).toBe("pipeline");
  });

  it("uses the asset id when a document id is absent", () => {
    const asset: LibAsset = {
      asset: { id: "a1", kind: "text", path: "/tmp/a.md" },
      source: "分镜",
      params: { text: "普通文本" },
      createdAt: 1,
    };
    const meta = getDocumentMeta(asset)!;
    expect(meta.documentType).toBe("storyboard");
    expect(meta.version).toBe(1);
    expect(meta.documentId).toBe("a1");
    expect(meta.shots).toEqual([]);
  });

  it("groups explicit document versions into one chronological history", () => {
    const original: LibAsset = { asset: { id: "a1", kind: "text", path: "/a.md" }, source: "分镜", projectId: "p1", params: { text: "one", documentId: "doc-1", version: 1 }, createdAt: 1 };
    const copy: LibAsset = { asset: { id: "a2", kind: "text", path: "/b.md" }, source: "分镜 · 副本", projectId: "p1", params: { text: "two", documentId: "doc-1", version: 2 }, createdAt: 2 };
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

  it("orders the production head without treating a historical branch as current", () => {
    const current: LibAsset = { asset: { id: "current", kind: "text", path: "/current.md" }, source: "剧本", params: { text: "current", documentId: "doc", version: 2 }, createdAt: 2 };
    const branch: LibAsset = { asset: { id: "branch", kind: "text", path: "/branch.md" }, source: "剧本 · 分支", params: { text: "branch", documentId: "doc", version: 3, videoBranch: true }, createdAt: 3 };
    expect(getDocumentVersions(current, [branch, current]).map((asset) => asset.asset.id)).toEqual(["branch", "current"]);
    expect([branch, current].filter((asset) => asset.params?.videoBranch !== true).sort((left, right) => right.createdAt - left.createdAt)[0].asset.id).toBe("current");
  });

  it("saves through the atomic backend command with the expected production head", async () => {
    const parent: LibAsset = {
      asset: { id: "parent", kind: "text", path: "/parent.md" },
      source: "视频剧本",
      projectId: "p1",
      params: { text: "旧版本", title: "视频剧本", documentType: "script", documentId: "doc-1", version: 1 },
      createdAt: 1,
    };
    useLibraryStore.getState().loadAssets([parent]);
    vi.mocked(saveDocumentVersionAtomic).mockResolvedValue({
      asset: { id: "saved", kind: "text", path: "/saved.md", format: "md" },
      params: { text: "新版本", title: "视频剧本", documentType: "script", documentId: "doc-1", version: 2, videoBranch: false },
      version: 2,
    });

    const saved = await saveDocumentVersion({
      title: "视频剧本",
      text: "新版本",
      projectId: "p1",
      documentType: "script",
      parent,
      changeType: "manual",
      expectedHeadAssetId: "parent",
    });

    expect(saveDocumentVersionAtomic).toHaveBeenCalledWith(expect.objectContaining({
      documentId: "doc-1",
      expectedHeadAssetId: "parent",
      allowBranch: undefined,
    }));
    expect(saved.asset.id).toBe("saved");
    expect(useLibraryStore.getState().assets[0].params?.version).toBe(2);
  });

  it("lets the backend mark an explicitly requested historical branch", async () => {
    const parent: LibAsset = {
      asset: { id: "parent", kind: "text", path: "/parent.md" },
      source: "视频剧本",
      projectId: "p1",
      params: { text: "旧版本", title: "视频剧本", documentType: "script", documentId: "doc-1", version: 1 },
      createdAt: 1,
    };
    useLibraryStore.getState().loadAssets([parent]);
    vi.mocked(saveDocumentVersionAtomic).mockResolvedValue({
      asset: { id: "branch", kind: "text", path: "/branch.md", format: "md" },
      params: { text: "历史分支", title: "视频剧本", documentType: "script", documentId: "doc-1", version: 2, videoBranch: true },
      version: 2,
    });

    await saveDocumentVersion({
      title: "视频剧本",
      text: "历史分支",
      projectId: "p1",
      documentType: "script",
      parent,
      changeType: "manual",
      expectedHeadAssetId: "parent",
      allowBranch: true,
    });

    expect(saveDocumentVersionAtomic).toHaveBeenCalledWith(expect.objectContaining({
      expectedHeadAssetId: undefined,
      allowBranch: true,
    }));
    expect(useLibraryStore.getState().assets[0].params?.videoBranch).toBe(true);
  });
});
