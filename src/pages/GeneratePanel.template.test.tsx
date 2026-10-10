// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { libraryImportEntry } from "../lib/assetImport";
import { useGenerationStore } from "../store/useGenerationStore";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { useRunStore } from "../store/useRunStore";
import GeneratePanel from "./GeneratePanel";

const mocks = vi.hoisted(() => ({
  generateImage: vi.fn(),
  libraryFails: false,
  pickerInput: null as unknown,
  template: {
    id: "tpl-1", kind: "template" as const, title: "电影海报", prompt: "模板提示词", category: "poster",
    styles: [], scenes: [], tags: [], image: "", guidance: ["保留主体"],
  },
}));

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
vi.mock("../lib/generate", () => ({ generateImage: mocks.generateImage }));
// The library and the picker have their own tests; here they only hand a
// choice to the panel, the way their real buttons do.
vi.mock("../components/PromptLibrary", () => ({
  default: ({ open, onPick }: { open: boolean; onPick: (pick: unknown) => void }) => {
    if (mocks.libraryFails) throw new Error("模板库代码加载失败");
    return open ? <button type="button" onClick={() => onPick({ prompt: mocks.template.prompt, entry: mocks.template })}>模拟选用模板</button> : null;
  },
}));
vi.mock("../components/AssetImportPicker", () => ({
  default: ({ open, onApply }: { open: boolean; onApply: (input: unknown) => void }) => open
    ? <button type="button" onClick={() => onApply(mocks.pickerInput)}>模拟应用导入</button>
    : null,
}));

const promptBox = () => screen.getByPlaceholderText(/描述你想生成的画面/) as HTMLTextAreaElement;
const templateLine = () => screen.queryByText(/^当前模板：/);

beforeEach(() => {
  vi.clearAllMocks();
  mocks.generateImage.mockResolvedValue([]);
  mocks.libraryFails = false;
  mocks.pickerInput = null;
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
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("GeneratePanel prompt template", () => {
  it("keeps the picked template with its prompt after leaving the tab and records it on the run", async () => {
    const first = render(<GeneratePanel />);
    fireEvent.click(screen.getByRole("button", { name: "模板库" }));
    fireEvent.click(await screen.findByRole("button", { name: "模拟选用模板" }));
    expect(templateLine()?.textContent).toBe("当前模板：电影海报");
    // Switching tabs unmounts the panel.
    first.unmount();

    render(<GeneratePanel />);
    expect(promptBox().value).toBe("模板提示词");
    expect(templateLine()?.textContent).toBe("当前模板：电影海报");

    fireEvent.click(screen.getByRole("button", { name: "生成" }));
    await waitFor(() => expect(mocks.generateImage).toHaveBeenCalled());
    expect(mocks.generateImage.mock.calls[0][1]).toMatchObject({
      sourceMaterials: [{ kind: "context", label: "提示词模板 · 电影海报", source: "tpl-1" }],
    });
  });

  it("drops the template when the prompt is edited by hand", () => {
    useGenerationStore.getState().set({ prompt: "模板提示词", promptTemplate: mocks.template });
    render(<GeneratePanel />);
    expect(templateLine()).toBeTruthy();

    fireEvent.change(promptBox(), { target: { value: "模板提示词，再加一句" } });

    expect(templateLine()).toBeNull();
    expect(useGenerationStore.getState().promptTemplate).toBeNull();
  });

  it("drops the template when an imported prompt replaces it", () => {
    useGenerationStore.getState().set({ prompt: "模板提示词", promptTemplate: mocks.template });
    const document: LibAsset = { asset: { id: "doc", kind: "text", path: "C:/doc.md" }, source: "文档", projectId: "p1", params: { text: "导入的正文" }, createdAt: 1 };
    mocks.pickerInput = { action: "prompt", referenceMode: "replace", entries: [libraryImportEntry(document)] };
    render(<GeneratePanel />);

    fireEvent.click(screen.getByRole("button", { name: "从资产库导入" }));
    fireEvent.click(screen.getByRole("button", { name: "模拟应用导入" }));

    expect(promptBox().value).toBe("导入的正文");
    expect(templateLine()).toBeNull();
  });
});

describe("GeneratePanel keyboard access", () => {
  it("closes the template library load error with Escape", async () => {
    mocks.libraryFails = true;
    // React reports the error the boundary catches.
    vi.spyOn(console, "error").mockImplementation(() => {});
    render(<GeneratePanel />);

    fireEvent.click(screen.getByRole("button", { name: "模板库" }));
    const dialog = await screen.findByRole("dialog", { name: "模板库加载失败" });
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "返回生成页" }));

    fireEvent.keyDown(document.body, { key: "Escape" });

    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("removes one reference thumbnail with the keyboard", async () => {
    useGenerationStore.getState().set({
      referencePath: "C:/a.png",
      references: [{ path: "C:/a.png", role: "base_image", sortOrder: 0 }, { path: "C:/b.png", role: "base_image", sortOrder: 1 }],
    });
    render(<GeneratePanel />);
    const remove = screen.getByRole("button", { name: "移除参考图 2" });
    // display:none would take it out of the tab order; it is only faded until hover or focus.
    expect(remove.className.split(" ")).not.toContain("hidden");
    expect(screen.getByRole("button", { name: "移除全部参考图" })).toBeTruthy();

    remove.focus();
    await userEvent.setup().keyboard("{Enter}");

    expect(useGenerationStore.getState().references?.map((item) => item.path)).toEqual(["C:/a.png"]);
  });
});
