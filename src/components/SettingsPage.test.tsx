// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import SettingsPage from "./SettingsPage";
import * as settingsLib from "../lib/settings";
import { confirmAction } from "../lib/confirm";
import { fetchModels } from "../lib/ipc";

vi.mock("../lib/desktop", () => ({ openDialog: vi.fn() }));
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

const backup = () => ({
  id: "p2",
  name: "备用线路",
  image: { url: "https://img2.api/v1/images/generations", key: "sk-img2", model: "gpt-image-2" },
  video: { url: "https://vid2.api/v1/videos", key: "sk-vid2", model: "kling-video-v3" },
});

/** Two profiles, the first one default; the dialog opens on it. */
const loadTwoProfiles = () =>
  vi.spyOn(settingsLib, "loadSettings").mockResolvedValue({
    configs: [profile(), backup()],
    activeId: "p1",
    outputDir: "D:/assets",
    llmUrl: "https://llm.api/v1",
    llmKey: "",
    llmModel: "gemini-3.7-flash",
  });

const button = (name: string | RegExp) => screen.getByRole("button", { name }) as HTMLButtonElement;
const saveButton = () => button(/保存配置/);
/** The select button of a profile row; its name starts with the profile name. */
const row = (name: string) => button(new RegExp(`^${name}`));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.mocked(fetchModels).mockReset();
  vi.mocked(confirmAction).mockClear();
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
      // Nothing was deleted: with no profile the backend may hold .env/REST
      // credentials, which a plain save must not clear.
      expect(saveSpy).toHaveBeenCalledWith(expect.objectContaining({
        outputDir: "D:/new/assets",
        llmUrl: "https://api.initial/v1",
        configs: [],
      }), { clearConnections: false });
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
      }), { clearConnections: false });
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

  it("enables saving only with changes and shows the result beside the save button until the next edit", async () => {
    loadTwoProfiles();
    const saveSpy = vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());
    expect(saveButton().disabled).toBe(true);

    fireEvent.change(screen.getByDisplayValue("默认配置"), { target: { value: "主线路" } });
    expect(saveButton().disabled).toBe(false);
    fireEvent.click(saveButton());

    await waitFor(() => expect(screen.getByRole("status").textContent).toBe("已保存"));
    expect(saveSpy).toHaveBeenCalledOnce();
    // The note sits in the footer, not in the scrolling form.
    expect(screen.getByRole("status").closest("footer")?.contains(saveButton())).toBe(true);
    expect(saveButton().disabled).toBe(true);

    fireEvent.change(screen.getByDisplayValue("https://llm.api/v1"), { target: { value: "https://llm2.api/v1" } });
    expect(screen.getByRole("status").textContent).toBe("");
    expect(saveButton().disabled).toBe(false);
  });

  it("shows no form until the settings are read, and offers a retry instead of an empty form when reading fails", async () => {
    const load = vi.spyOn(settingsLib, "loadSettings").mockRejectedValueOnce(new Error("database is locked"));
    const saveSpy = vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    expect(screen.getByText("正在读取配置…")).toBeTruthy();
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "读取配置失败：database is locked");
    expect(screen.queryByDisplayValue("D:/assets")).toBeNull();
    expect(saveButton().disabled).toBe(true);
    expect(button("新增配置").disabled).toBe(true);
    fireEvent.keyDown(window, { key: "s", ctrlKey: true });
    expect(saveSpy).not.toHaveBeenCalled();

    load.mockResolvedValue({ configs: [profile(), backup()], activeId: "p1", outputDir: "D:/assets", llmUrl: "", llmKey: "", llmModel: "gemini-3.7-flash" });
    fireEvent.click(button("重试"));
    expect(await screen.findByDisplayValue("默认配置")).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("marks the first profile as default when none is chosen, as the backend does", async () => {
    vi.spyOn(settingsLib, "loadSettings").mockResolvedValue({ configs: [profile(), backup()], activeId: null, outputDir: "", llmUrl: "", llmKey: "", llmModel: "gemini-3.7-flash" });
    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());
    expect(button(/已是默认/).disabled).toBe(true);
    fireEvent.click(row("备用线路"));
    expect(button(/^设为默认$/).disabled).toBe(false);
  });

  it("keeps edits made while a save is in flight unsaved", async () => {
    loadTwoProfiles();
    let finish!: () => void;
    vi.spyOn(settingsLib, "saveSettings").mockReturnValue(new Promise((resolve) => { finish = () => resolve(null); }));

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());
    fireEvent.change(screen.getByDisplayValue("默认配置"), { target: { value: "主线路" } });
    fireEvent.click(saveButton());
    fireEvent.change(screen.getByDisplayValue("https://llm.api/v1"), { target: { value: "https://llm2.api/v1" } });

    await act(async () => finish());
    expect(saveButton().disabled).toBe(false);
    expect(screen.getByRole("status").textContent).toBe("");
  });

  it("lets the success note fade after a few seconds", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      loadTwoProfiles();
      vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);
      render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
      await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());

      fireEvent.change(screen.getByDisplayValue("默认配置"), { target: { value: "主线路" } });
      fireEvent.click(saveButton());
      await waitFor(() => expect(screen.getByRole("status").textContent).toBe("已保存"));

      // Let the status effect arm its timer before moving the clock.
      await act(async () => {});
      act(() => { vi.advanceTimersByTime(3000); });
      expect(screen.getByRole("status").textContent).toBe("已保存");
      act(() => { vi.advanceTimersByTime(2000); });
      expect(screen.getByRole("status").textContent).toBe("");
    } finally {
      vi.useRealTimers();
    }
  });

  it("disables 设为默认 on the current default and names the profile it promotes", async () => {
    loadTwoProfiles();
    const saveSpy = vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());
    expect(button("已是默认").disabled).toBe(true);

    fireEvent.click(row("备用线路"));
    fireEvent.click(button("设为默认"));

    await waitFor(() => expect(screen.getByRole("status").textContent).toBe("已将“备用线路”设为默认"));
    expect(saveSpy).toHaveBeenCalledWith(expect.objectContaining({ activeId: "p2" }), { clearConnections: false });
    expect(button("已是默认").disabled).toBe(true);
  });

  it("says 设为默认并保存 when other edits are pending, and saves them with the new default", async () => {
    loadTwoProfiles();
    const saveSpy = vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());

    fireEvent.click(row("备用线路"));
    expect(screen.queryByRole("button", { name: "设为默认并保存" })).toBeNull();
    fireEvent.change(screen.getByDisplayValue("https://llm.api/v1"), { target: { value: "https://llm2.api/v1" } });
    fireEvent.click(button("设为默认并保存"));

    await waitFor(() => expect(screen.getByRole("status").textContent).toBe("已将“备用线路”设为默认"));
    expect(saveSpy).toHaveBeenCalledWith(
      expect.objectContaining({ activeId: "p2", llmUrl: "https://llm2.api/v1" }),
      { clearConnections: false },
    );
    expect(saveButton().disabled).toBe(true);
  });

  it("shows a failed save once and without an Error: prefix, and keeps the old default", async () => {
    loadTwoProfiles();
    vi.spyOn(settingsLib, "saveSettings").mockRejectedValue(new Error("磁盘已满"));

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());

    fireEvent.change(screen.getByDisplayValue("默认配置"), { target: { value: "主线路" } });
    fireEvent.click(saveButton());
    expect((await screen.findByRole("alert")).textContent).toBe("保存失败：磁盘已满");
    expect(screen.getAllByRole("alert")).toHaveLength(1);
    expect(screen.getByRole("status").textContent).toBe("");
    // An error stays while the user fixes things, until the next attempt.
    fireEvent.change(screen.getByDisplayValue("主线路"), { target: { value: "主线路 2" } });
    expect(screen.getByRole("alert").textContent).toBe("保存失败：磁盘已满");

    fireEvent.click(row("备用线路"));
    fireEvent.click(button("设为默认并保存"));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toBe("设为默认失败：磁盘已满"));
    expect(screen.getAllByRole("alert")).toHaveLength(1);
    expect(screen.getByRole("alert").textContent).not.toContain("Error");
    // The star stays where the backend still has it.
    expect(button("设为默认并保存").disabled).toBe(false);
    expect(row("主线路 2").textContent).toContain("默认");
  });

  it("saves on Ctrl/Cmd+S only when there is something to save", async () => {
    loadTwoProfiles();
    const saveSpy = vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());

    fireEvent.keyDown(window, { key: "s", ctrlKey: true });
    expect(saveSpy).not.toHaveBeenCalled();

    fireEvent.change(screen.getByDisplayValue("默认配置"), { target: { value: "主线路" } });
    fireEvent.keyDown(screen.getByDisplayValue("主线路"), { key: "s", metaKey: true });
    await waitFor(() => expect(screen.getByRole("status").textContent).toBe("已保存"));
    expect(saveSpy).toHaveBeenCalledOnce();

    fireEvent.keyDown(window, { key: "s", ctrlKey: true });
    expect(saveSpy).toHaveBeenCalledOnce();
  });

  it("closes on Escape through the unsaved-changes guard, but not while a model list is open", async () => {
    loadTwoProfiles();
    vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);
    const onClose = vi.fn();

    render(<SettingsPage open onClose={onClose} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());

    // Escape in an open dropdown only closes the dropdown.
    const model = screen.getByRole("combobox", { name: "图像模型" });
    fireEvent.click(model);
    expect(screen.getByRole("listbox")).toBeTruthy();
    fireEvent.keyDown(model, { key: "Escape" });
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.keyDown(model, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
    expect(confirmAction).not.toHaveBeenCalled();

    fireEvent.change(screen.getByDisplayValue("默认配置"), { target: { value: "主线路" } });
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    await act(async () => { fireEvent.keyDown(window, { key: "Escape" }); });
    expect(confirmAction).toHaveBeenCalledWith("接口配置有未保存修改，确定关闭吗？");
    expect(onClose).toHaveBeenCalledOnce();

    await act(async () => { fireEvent.keyDown(window, { key: "Escape" }); });
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("is a labelled dialog whose profile rows work from the keyboard", async () => {
    loadTwoProfiles();
    vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);
    const user = userEvent.setup();

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());

    expect(screen.getByRole("dialog", { name: "设置" }).getAttribute("aria-modal")).toBe("true");
    expect(button("关闭")).toBeTruthy();
    expect(button("新增配置")).toBeTruthy();
    // The delete button is a sibling of the row's select button, not inside it.
    expect(row("备用线路").querySelector("button")).toBeNull();
    expect(button("删除配置“备用线路”")).toBeTruthy();

    row("备用线路").focus();
    await user.keyboard("{Enter}");
    expect(screen.getByDisplayValue("备用线路")).toBeTruthy();
    expect(row("备用线路").getAttribute("aria-current")).toBe("true");

    row("默认配置").focus();
    await user.keyboard(" ");
    expect(screen.getByDisplayValue("默认配置")).toBeTruthy();
  });

  it("asks before deleting a profile and points at 保存配置 to apply it", async () => {
    loadTwoProfiles();
    const saveSpy = vi.spyOn(settingsLib, "saveSettings").mockResolvedValue(null);

    render(<SettingsPage open onClose={vi.fn()} onSaved={vi.fn()} status={null} />);
    await waitFor(() => expect(screen.getByDisplayValue("默认配置")).toBeTruthy());

    fireEvent.click(button("删除配置“备用线路”"));
    await waitFor(() => expect(screen.queryByRole("button", { name: /^备用线路/ })).toBeNull());
    expect(vi.mocked(confirmAction).mock.calls[0][0]).toContain("「保存配置」后生效");

    fireEvent.click(saveButton());
    await waitFor(() => {
      expect(saveSpy).toHaveBeenCalledWith(expect.objectContaining({ configs: [expect.objectContaining({ id: "p1" })] }), {
        clearConnections: true,
      });
    });
  });
});
