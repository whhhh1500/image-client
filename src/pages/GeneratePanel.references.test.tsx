// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { libraryImportEntry } from "../lib/assetImport";
import { useGenerationStore } from "../store/useGenerationStore";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { useRunStore } from "../store/useRunStore";
import GeneratePanel from "./GeneratePanel";

/** What the stubbed asset picker hands to the panel when "applied". */
const picker = vi.hoisted(() => ({ input: null as unknown }));

vi.mock("../lib/desktop", () => ({
  fileSrc: (path: string) => path,
  openDialog: vi.fn(),
  listen: vi.fn(async () => () => {}),
  revealItemInDir: vi.fn(),
}));
vi.mock("../lib/ipc", () => ({
  fetchModels: vi.fn(),
  runNode: vi.fn(),
  saveMediaAsset: vi.fn(),
  inspectImageFile: vi.fn().mockResolvedValue({ path: "", directory: "", fileName: "", bytes: 0, kb: 0, displayPath: "", variants: [] }),
}));
vi.mock("../lib/dbWrite", () => ({ persistAssets: vi.fn().mockResolvedValue(undefined), persistTask: vi.fn().mockResolvedValue(undefined) }));
vi.mock("../lib/comic/markdownApi", () => ({ comicMdCatalogList: vi.fn().mockResolvedValue([]) }));
// The picker and the detail view have their own tests; here they only hand an
// action to the panel, the way their real buttons do.
vi.mock("../components/AssetImportPicker", () => ({
  default: ({ open, onApply }: { open: boolean; onApply: (input: unknown) => void }) => open
    ? <button type="button" onClick={() => onApply(picker.input)}>模拟应用导入</button>
    : null,
}));
vi.mock("../components/AssetDetailModal", () => ({
  default: ({ asset, onUseReference }: { asset: LibAsset | null; onUseReference?: (asset: LibAsset) => void }) => asset && onUseReference
    ? <button type="button" onClick={() => onUseReference(asset)}>详情添加为参考图</button>
    : null,
}));

function image(id: string): LibAsset {
  return { asset: { id, kind: "image", path: `C:/${id}.png` }, source: `历史图 ${id}`, projectId: "p1", createdAt: 1 };
}

function text(id: string, body?: string): LibAsset {
  return { asset: { id, kind: "text", path: `C:/${id}.md` }, source: `文档 ${id}`, projectId: "p1", params: body ? { text: body } : {}, createdAt: 1 };
}

const referencePaths = () => (useGenerationStore.getState().references ?? []).map((item) => item.path);

beforeEach(() => {
  vi.clearAllMocks();
  useGenerationStore.getState().reset();
  useLibraryStore.setState({ assets: [], tasks: [] });
  useRunStore.setState({ imageBusy: false, imageError: null, imageBatchIds: [], imageProgress: null, imageRunStartedAt: null, imageLog: [] });
  useProjectStore.setState({
    activeId: "p1",
    projects: [{
      id: "p1", name: "项目", description: "", storyStyle: "", artStyle: "", aspectRatio: "16:9",
      imageModel: "gpt-image-2", imageQuality: "high", videoModel: "kling-video-v3", videoResolution: "720p",
    }],
  });
  picker.input = null;
});

afterEach(cleanup);

describe("GeneratePanel single-asset reference actions", () => {
  it("appends a history image from the context menu and skips one that is already a reference", () => {
    useLibraryStore.setState({ assets: [image("added")], tasks: [] });
    useGenerationStore.getState().set({ referencePath: "C:/existing.png", references: [{ path: "C:/existing.png", role: "base_image", sortOrder: 0 }] });
    render(<GeneratePanel />);

    fireEvent.contextMenu(screen.getByAltText("历史图 added"));
    fireEvent.click(screen.getByRole("menuitem", { name: "添加为参考图" }));

    expect(referencePaths()).toEqual(["C:/existing.png", "C:/added.png"]);
    expect(useGenerationStore.getState().referencePath).toBe("C:/existing.png");
    expect(useGenerationStore.getState().importedSources).toEqual([expect.objectContaining({ action: "reference", assetIds: ["added"] })]);

    fireEvent.contextMenu(screen.getByAltText("历史图 added"));
    fireEvent.click(screen.getByRole("menuitem", { name: "添加为参考图" }));

    expect(referencePaths()).toEqual(["C:/existing.png", "C:/added.png"]);
    expect(useGenerationStore.getState().importedSources).toHaveLength(1);
    expect(screen.getByRole("status").textContent).toContain("已在参考图中");
  });

  it("appends from the detail view as well instead of replacing the references", () => {
    useLibraryStore.setState({ assets: [image("first"), image("second")], tasks: [] });
    render(<GeneratePanel />);

    fireEvent.click(screen.getAllByAltText("历史图 first")[0]);
    fireEvent.click(screen.getByRole("button", { name: "详情添加为参考图" }));
    fireEvent.click(screen.getAllByAltText("历史图 second")[0]);
    fireEvent.click(screen.getByRole("button", { name: "详情添加为参考图" }));

    expect(referencePaths()).toEqual(["C:/first.png", "C:/second.png"]);
    expect(useGenerationStore.getState().references?.map((item) => item.sortOrder)).toEqual([0, 1]);
  });
});

describe("GeneratePanel import notice", () => {
  it("shows a partial import as a neutral notice beside the run error and clears it on the next import", () => {
    useRunStore.setState({ imageError: "上次生成失败：503" });
    const usable = text("usable", "可用正文");
    picker.input = { action: "prompt", referenceMode: "replace", entries: [libraryImportEntry(usable), libraryImportEntry(text("empty"))] };
    render(<GeneratePanel />);

    fireEvent.click(screen.getByRole("button", { name: "从资产库导入" }));
    fireEvent.click(screen.getByRole("button", { name: "模拟应用导入" }));

    expect(useGenerationStore.getState().prompt).toBe("可用正文");
    const notice = screen.getByRole("status");
    expect(notice.textContent).toBe("已排除 1 项没有可见正文/已保存提示词的资源：文档 empty");
    expect(notice.className).not.toContain("rose");
    // The warning used to take the error box and hide the run's failure.
    const errors = screen.getAllByRole("alert");
    expect(errors.map((item) => item.textContent)).toEqual(["上次生成失败：503"]);

    picker.input = { action: "prompt", referenceMode: "replace", entries: [libraryImportEntry(usable)] };
    fireEvent.click(screen.getByRole("button", { name: "模拟应用导入" }));

    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.getByRole("alert").textContent).toBe("上次生成失败：503");
  });
});
