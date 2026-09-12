// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { fetchModels } from "../lib/ipc";
import AssetDetailModal from "./AssetDetailModal";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";

vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (path: string) => path }));
vi.mock("../lib/ipc", () => ({ fetchModels: vi.fn(), llmChat: vi.fn(), readTextAsset: vi.fn() }));
vi.mock("../lib/dbWrite", () => ({ updateAssetMetadata: vi.fn() }));

const imageAsset = (): LibAsset => ({
  asset: { id: "a1", kind: "image", path: "D:/a.png" },
  source: "AI 生图",
  projectId: "p1",
  createdAt: 1,
  model: "gpt-image-2",
});

afterEach(() => {
  cleanup();
  vi.mocked(fetchModels).mockReset();
});

describe("AssetDetailModal optimization model", () => {
  it("offers a fetched catalogue and saves a hand-typed model name", async () => {
    useLibraryStore.setState({ assets: [imageAsset()], tasks: [] });
    vi.mocked(fetchModels).mockResolvedValue(["gemini-4-pro", "gemini-4-flash"]);
    render(<AssetDetailModal asset={imageAsset()} onClose={vi.fn()} />);

    const input = screen.getByRole("combobox", { name: "优化模型" }) as HTMLInputElement;
    expect(input.value).toBe("gemini-3.7-flash");

    fireEvent.click(screen.getByRole("button", { name: "获取模型（优化模型）" }));
    await screen.findByText(/已获取 2 个模型/);
    expect(vi.mocked(fetchModels)).toHaveBeenCalledWith({ url: "", key: "", kind: "llm" });

    fireEvent.change(input, { target: { value: "自填优化模型" } });
    expect((screen.getByRole("combobox", { name: "优化模型" }) as HTMLInputElement).value)
      .toBe("自填优化模型");
  });
});
