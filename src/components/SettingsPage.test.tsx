// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import SettingsPage from "./SettingsPage";
import * as settingsLib from "../lib/settings";
import { fetchModels } from "../lib/ipc";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("../lib/confirm", () => ({ confirmAction: vi.fn().mockResolvedValue(true) }));
vi.mock("../lib/ipc", () => ({
  listVideoModels: vi.fn().mockResolvedValue(["kling-video-v3"]),
  fetchModels: vi.fn(),
}));

const profile = () => ({
  id: "p1",
  name: "默认配置",
  image: { url: "https://img.api/v1/images/generations", key: "sk-img", model: "gpt-image-2" },
  video: { url: "https://vid.api/v1/videos", key: "sk-vid", model: "kling-video-v3" },
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.mocked(fetchModels).mockReset();
});

describe("SettingsPage", () => {
  it("allows configuring and saving global settings when no profiles exist", async () => {
    vi.spyOn(settingsLib, "loadSettings").mockResolvedValue({
      configs: [],
      activeId: null,
      outputDir: "D:/initial/assets",
      llmUrl: "https://api.initial/v1",
      llmKey: "",
      llmModel: "gemini-3.7-flash",
    });
    const saveSpy = vi.spyOn(settingsLib, "saveSettings").mockResolvedValue({
      imageReady: false,
      videoReady: false,
      llmReady: true,
      imageModel: "gpt-image-2",
      videoModel: "kling-video-v3",
      llmModel: "gemini-3.7-flash",
      outputDir: "D:/new/assets",
      source: "db",
    });
    const onSaved = vi.fn();

    render(<SettingsPage open onClose={vi.fn()} onSaved={onSaved} status={null} />);

    await waitFor(() => expect(screen.getByDisplayValue("D:/initial/assets")).toBeTruthy());
    expect(screen.getAllByText("全局通用设置").length).toBeGreaterThanOrEqual(1);

    const outputDirInput = screen.getByDisplayValue("D:/initial/assets");
    fireEvent.change(outputDirInput, { target: { value: "D:/new/assets" } });

    const saveButton = screen.getByRole("button", { name: /保存配置/ });
    expect((saveButton as HTMLButtonElement).disabled).toBe(false);

    fireEvent.click(saveButton);

    await waitFor(() => {
      expect(saveSpy).toHaveBeenCalledWith(expect.objectContaining({
        outputDir: "D:/new/assets",
        llmUrl: "https://api.initial/v1",
        configs: [],
      }));
    });
    expect(onSaved).toHaveBeenCalled();
  });

  it("switches between global settings view and profile edit view", async () => {
    const profile = {
      id: "p1",
      name: "默认配置",
      image: { url: "https://img.api", key: "sk-img", model: "gpt-image-2" },
      video: { url: "https://vid.api", key: "sk-vid", model: "kling-video-v3" },
    };
    vi.spyOn(settingsLib, "loadSettings").mockResolvedValue({
      configs: [profile],
      activeId: "p1",
      outputDir: "D:/assets",
      llmUrl: "https://llm.api/v1",
      llmKey: "",
      llmModel: "gemini-3.7-flash",
    });
    vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);

    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());

    // Switch to global settings
    fireEvent.click(screen.getByText("全局通用设置"));
    expect(screen.getByText("全局通用配置项，所有生成任务与剧本/分镜 Agent 共享。")).toBeTruthy();

    // Switch back to profile
    fireEvent.click(screen.getByText("默认配置"));
    expect(screen.getByDisplayValue("默认配置")).toBeTruthy();
  });

  it("fetches the model list for the edited address and accepts a hand-typed model", async () => {
    vi.spyOn(settingsLib, "loadSettings").mockResolvedValue({
      configs: [profile()],
      activeId: "p1",
      outputDir: "D:/assets",
      llmUrl: "https://llm.api/v1",
      llmKey: "",
      llmModel: "gemini-3.7-flash",
    });
    const saveSpy = vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);
    vi.mocked(fetchModels).mockResolvedValue(["gpt-image-4", "brand-new-image"]);

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "获取模型（图像模型）" }));

    // The request carries exactly what the form holds, unsaved.
    await waitFor(() => {
      expect(vi.mocked(fetchModels)).toHaveBeenCalledWith({
        url: "https://img.api/v1/images/generations",
        key: "sk-img",
        kind: "image",
      });
    });
    expect(await screen.findByText(/已获取 2 个模型/)).toBeTruthy();

    // The fetched catalog replaces the dropdown list.
    fireEvent.click(screen.getByRole("button", { name: "展开图像模型列表" }));
    expect(screen.queryByRole("option", { name: "gpt-image-2" })).toBeNull();
    fireEvent.click(screen.getByRole("option", { name: "brand-new-image" }));
    expect((screen.getByRole("combobox", { name: "图像模型" }) as HTMLInputElement).value)
      .toBe("brand-new-image");

    // ... and a name that never appeared in any list still saves.
    fireEvent.change(screen.getByRole("combobox", { name: "图像模型" }), {
      target: { value: "自填模型-v9" },
    });
    fireEvent.click(screen.getByRole("button", { name: /保存配置/ }));

    await waitFor(() => {
      expect(saveSpy).toHaveBeenCalledWith(expect.objectContaining({
        configs: [expect.objectContaining({
          image: expect.objectContaining({ model: "自填模型-v9" }),
        })],
      }));
    });
  });

  it("surfaces a failed model fetch without touching the configured model", async () => {
    vi.spyOn(settingsLib, "loadSettings").mockResolvedValue({
      configs: [profile()],
      activeId: "p1",
      outputDir: "D:/assets",
      llmUrl: "",
      llmKey: "",
      llmModel: "gemini-3.7-flash",
    });
    vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);
    vi.mocked(fetchModels).mockRejectedValue("模型目录返回 401 Unauthorized，请检查 API Key、权限和地址");

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "获取模型（图像模型）" }));

    expect(await screen.findByText(/获取模型失败：模型目录返回 401 Unauthorized/)).toBeTruthy();
    expect((screen.getByRole("combobox", { name: "图像模型" }) as HTMLInputElement).value)
      .toBe("gpt-image-2");
  });

  it("fetches the LLM catalog from the global settings view", async () => {
    vi.spyOn(settingsLib, "loadSettings").mockResolvedValue({
      configs: [],
      activeId: null,
      outputDir: "D:/assets",
      llmUrl: "https://llm.api/v1",
      llmKey: "",
      llmModel: "gemini-3.7-flash",
    });
    vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);
    vi.mocked(fetchModels).mockResolvedValue(["gemini-4-pro"]);

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("D:/assets")).toBeTruthy());

    // The form never holds the stored LLM key, so the backend borrows it.
    fireEvent.click(screen.getByRole("button", { name: "获取模型（LLM 模型）" }));

    await waitFor(() => {
      expect(vi.mocked(fetchModels)).toHaveBeenCalledWith({
        url: "https://llm.api/v1",
        key: "",
        kind: "llm",
      });
    });
    await waitFor(() => {
      fireEvent.click(screen.getByRole("button", { name: "展开LLM 模型列表" }));
      expect(screen.getByRole("option", { name: "gemini-4-pro" })).toBeTruthy();
    });
  });
});
