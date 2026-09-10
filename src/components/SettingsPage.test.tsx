// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import SettingsPage from "./SettingsPage";
import * as settingsLib from "../lib/settings";

vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("../lib/confirm", () => ({ confirmAction: vi.fn().mockResolvedValue(true) }));
vi.mock("../lib/ipc", () => ({ listVideoModels: vi.fn().mockResolvedValue(["kling-video-v3"]) }));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
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
});
