// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { configStatus } from "../lib/ipc";
import { confirmAction } from "../lib/confirm";
import AssetPicker from "./AssetPicker";
import AssetDetailModal from "./AssetDetailModal";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";

vi.mock("../lib/desktop", () => ({ fileSrc: (path: string) => path }));
vi.mock("../lib/ipc", () => ({ configStatus: vi.fn(), fetchModels: vi.fn(), llmChat: vi.fn(), readTextAsset: vi.fn() }));
vi.mock("../lib/dbWrite", () => ({ updateAssetMetadata: vi.fn() }));
vi.mock("../lib/confirm", () => ({ confirmAction: vi.fn() }));

const image: LibAsset = { asset: { id: "a1", kind: "image", path: "D:/a.png" }, source: "雨夜街道", projectId: "p1", createdAt: 1, params: { prompt: "雨夜" } };
const configured = { imageReady: true, videoReady: true, llmReady: true, imageModel: "", videoModel: "", llmModel: "", outputDir: "", source: "db" as const };
const pressEscape = () => fireEvent.keyDown(window, { key: "Escape" });

afterEach(() => {
  cleanup();
  vi.mocked(confirmAction).mockReset();
  useLibraryStore.setState({ assets: [], tasks: [] });
});

describe("AssetPicker keyboard", () => {
  it("closes the preview opened from the picker first, then the picker", async () => {
    vi.mocked(configStatus).mockResolvedValue(configured);
    useProjectStore.setState({ activeId: "p1" });
    useLibraryStore.setState({ assets: [image], tasks: [] });
    const onClose = vi.fn();
    render(<AssetPicker open onClose={onClose} onPick={vi.fn()} kinds={["image"]} title="导入参考图" />);

    expect(document.activeElement).toBe(screen.getByPlaceholderText("搜索标题、内容、模型或提示词"));
    fireEvent.click(screen.getByRole("button", { name: "雨夜街道" }));
    await screen.findByRole("dialog", { name: "雨夜街道" });

    pressEscape();
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "雨夜街道" })).toBeNull());
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "导入参考图" })).toBeTruthy();

    pressEscape();
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("AssetDetailModal keyboard", () => {
  it("sends Escape through the unsaved-change guard", async () => {
    vi.mocked(configStatus).mockResolvedValue(configured);
    useLibraryStore.setState({ assets: [image], tasks: [] });
    const onClose = vi.fn();
    render(<AssetDetailModal asset={image} onClose={onClose} />);
    const dialog = screen.getByRole("dialog", { name: "雨夜街道" });
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "关闭" }));

    // Nothing edited: Escape closes at once.
    pressEscape();
    expect(confirmAction).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);

    fireEvent.change(screen.getByDisplayValue("雨夜"), { target: { value: "雪夜" } });
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    pressEscape();
    await waitFor(() => expect(confirmAction).toHaveBeenCalledWith("当前有未保存修改，确定关闭吗？"));
    expect(onClose).toHaveBeenCalledTimes(1);

    vi.mocked(confirmAction).mockResolvedValueOnce(true);
    pressEscape();
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(2));
  });
});
