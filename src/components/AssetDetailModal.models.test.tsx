// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { configStatus, fetchModels, llmChat } from "../lib/ipc";
import AssetDetailModal from "./AssetDetailModal";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";

vi.mock("../lib/desktop", () => ({ fileSrc: (path: string) => path }));
vi.mock("../lib/ipc", () => ({ configStatus: vi.fn(), fetchModels: vi.fn(), llmChat: vi.fn(), readTextAsset: vi.fn() }));
vi.mock("../lib/dbWrite", () => ({ updateAssetMetadata: vi.fn() }));

const imageAsset = (params?: Record<string, unknown>): LibAsset => ({
  asset: { id: "a1", kind: "image", path: "D:/a.png" },
  source: "AI 生图",
  projectId: "p1",
  createdAt: 1,
  model: "gpt-image-2",
  ...(params ? { params } : {}),
});

const configured = (llmModel: string) => ({ imageReady: true, videoReady: true, llmReady: true, imageModel: "", videoModel: "", llmModel, outputDir: "", source: "db" as const });

afterEach(() => {
  cleanup();
  vi.mocked(fetchModels).mockReset();
  vi.mocked(llmChat).mockReset();
  vi.mocked(configStatus).mockReset();
});

describe("AssetDetailModal optimization model", () => {
  it("offers a fetched catalogue and saves a hand-typed model name", async () => {
    useLibraryStore.setState({ assets: [imageAsset()], tasks: [] });
    vi.mocked(configStatus).mockResolvedValue(configured("deepseek-v4"));
    vi.mocked(fetchModels).mockResolvedValue(["gemini-4-pro", "gemini-4-flash"]);
    render(<AssetDetailModal asset={imageAsset()} onClose={vi.fn()} />);

    const input = screen.getByRole("combobox", { name: "优化模型" }) as HTMLInputElement;
    expect(input.value).toBe("");

    fireEvent.click(screen.getByRole("button", { name: "获取模型（优化模型）" }));
    await screen.findByText(/已获取 2 个模型/);
    expect(vi.mocked(fetchModels)).toHaveBeenCalledWith({ url: "", key: "", kind: "llm" });

    fireEvent.change(input, { target: { value: "自填优化模型" } });
    expect((screen.getByRole("combobox", { name: "优化模型" }) as HTMLInputElement).value)
      .toBe("自填优化模型");
  });

  it("leaves the model to the backend's configured one and names it", async () => {
    vi.mocked(configStatus).mockResolvedValue(configured("deepseek-v4"));
    vi.mocked(llmChat).mockResolvedValue("更好的提示词");
    const asset = imageAsset({ prompt: "雨夜街道" });
    useLibraryStore.setState({ assets: [asset], tasks: [] });
    render(<AssetDetailModal asset={asset} onClose={vi.fn()} />);

    await screen.findByText("留空将使用设置中的文本模型（deepseek-v4）");
    expect(screen.getByRole("combobox", { name: "优化模型" }).getAttribute("placeholder")).toBe("默认：deepseek-v4");

    fireEvent.click(screen.getByRole("button", { name: "智能优化提示词" }));
    await waitFor(() => expect(llmChat).toHaveBeenCalledTimes(1));
    expect(vi.mocked(llmChat).mock.calls[0][2]).toBeUndefined();
    await screen.findByDisplayValue("更好的提示词");
  });

  it("passes a model only when the user picked one", async () => {
    vi.mocked(configStatus).mockResolvedValue(configured("deepseek-v4"));
    vi.mocked(llmChat).mockResolvedValue("更好的提示词");
    const asset = imageAsset({ prompt: "雨夜街道" });
    useLibraryStore.setState({ assets: [asset], tasks: [] });
    render(<AssetDetailModal asset={asset} onClose={vi.fn()} />);

    fireEvent.change(screen.getByRole("combobox", { name: "优化模型" }), { target: { value: "gemini-4-pro" } });
    fireEvent.click(screen.getByRole("button", { name: "智能优化提示词" }));
    await waitFor(() => expect(llmChat).toHaveBeenCalledTimes(1));
    expect(vi.mocked(llmChat).mock.calls[0][2]).toBe("gemini-4-pro");
  });

  it("shows a readable optimization error", async () => {
    vi.mocked(configStatus).mockRejectedValue("config unavailable");
    vi.mocked(llmChat).mockRejectedValue(new Error("未配置文本 LLM"));
    const asset = imageAsset({ prompt: "雨夜街道" });
    useLibraryStore.setState({ assets: [asset], tasks: [] });
    render(<AssetDetailModal asset={asset} onClose={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "智能优化提示词" }));
    expect(await screen.findByText("未配置文本 LLM")).toBeTruthy();
  });
});

describe("AssetDetailModal technical params", () => {
  it("keeps raw params collapsed and leaves the provenance out of them", async () => {
    vi.mocked(configStatus).mockResolvedValue(configured(""));
    const asset = imageAsset({
      prompt: "雨夜街道",
      size: "1024x1536",
      provenance: { schemaVersion: 1, recordedAt: 1, originalInput: "最初的想法", generationInput: "雨夜街道", sourceMaterials: [], parentAssetIds: [] },
    });
    useLibraryStore.setState({ assets: [asset], tasks: [] });
    const { container } = render(<AssetDetailModal asset={asset} onClose={vi.fn()} />);

    const summary = screen.getByText("技术参数");
    const details = summary.closest("details")!;
    expect(details.open).toBe(false);
    const dump = details.querySelector("pre")!.textContent!;
    expect(JSON.parse(dump)).toEqual({ prompt: "雨夜街道", size: "1024x1536" });
    // The provenance is still shown, once, in its own panel.
    expect(container.textContent).toContain("最初的想法");
  });
});
