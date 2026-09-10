// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { confirmAction } from "../lib/confirm";
import { listVideoModelCapabilities, listVideoModels } from "../lib/ipc";
import ProjectSettingsPage from "./ProjectSettingsPage";
import type { Project } from "../store/useProjectStore";

vi.mock("../lib/confirm", () => ({ confirmAction: vi.fn() }));
vi.mock("../lib/ipc", () => ({ listVideoModels: vi.fn(), listVideoModelCapabilities: vi.fn() }));
beforeEach(() => {
  vi.mocked(listVideoModels).mockResolvedValue(["grok-imagine-video", "minimax-h3"]);
  vi.mocked(listVideoModelCapabilities).mockResolvedValue([
    {
      id: "grok-imagine-video", label: "Grok Imagine Video", modes: ["text", "first_frame", "reference"],
      minDurationS: 1, maxDurationS: 15, durationOptions: [1, 2, 3], resolutions: ["480p", "720p", "1080p"],
      aspectRatios: ["16:9", "9:16", "1:1"], maxImages: 7, maxVideos: 0, maxAudios: 0, note: "",
    },
    {
      id: "minimax-h3", label: "MiniMax H3 · 2K", modes: ["text", "reference"],
      minDurationS: 5, maxDurationS: 15, durationOptions: [5, 6, 7], resolutions: ["2K"],
      aspectRatios: ["16:9", "9:16", "1:1"], maxImages: 5, maxVideos: 3, maxAudios: 1, note: "",
    },
  ]);
});
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it.each([false, true])("waits for confirmation and only closes on acceptance (%s)", async (accepted) => {
  let resolveConfirmation!: (value: boolean) => void;
  vi.mocked(confirmAction).mockReturnValue(new Promise<boolean>((resolve) => { resolveConfirmation = resolve; }));
  const onClose = vi.fn();
  const project: Project = {
    id: "test", name: "项目", description: "", storyStyle: "通用短剧", artStyle: "电影写实",
    aspectRatio: "16:9", imageModel: "gpt-image-2", imageQuality: "high", videoModel: "grok-imagine-video", videoResolution: "720p",
  };
  render(<ProjectSettingsPage open project={project} onClose={onClose} />);
  fireEvent.change(screen.getByLabelText("项目名称"), { target: { value: "未保存修改" } });
  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  expect(confirmAction).toHaveBeenCalledOnce();
  expect(onClose).not.toHaveBeenCalled();
  await act(async () => { resolveConfirmation(accepted); });
  expect(onClose).toHaveBeenCalledTimes(accepted ? 1 : 0);
  expect((screen.getByLabelText("项目名称") as HTMLInputElement).value).toBe("未保存修改");
});

it("only offers resolutions supported by the selected video model", async () => {
  const project: Project = {
    id: "test", name: "项目", description: "", storyStyle: "通用短剧", artStyle: "电影写实",
    aspectRatio: "16:9", imageModel: "gpt-image-2", imageQuality: "high", videoModel: "grok-imagine-video", videoResolution: "1080p",
  };
  render(<ProjectSettingsPage open project={project} onClose={vi.fn()} />);

  await waitFor(() => expect(screen.getByText(/Grok Imagine Video 支持/)).toBeTruthy());
  const model = screen.getByLabelText("默认视频模型") as HTMLSelectElement;
  const resolution = screen.getByLabelText("默认视频分辨率") as HTMLSelectElement;
  expect([...resolution.options].map((option) => option.value)).toEqual(["480p", "720p", "1080p"]);

  fireEvent.change(model, { target: { value: "minimax-h3" } });

  expect(resolution.value).toBe("2K");
  expect([...resolution.options].map((option) => option.value)).toEqual(["2K"]);
  expect(screen.getByRole("status").textContent).toContain("已切换为 2K");
});

it("keeps verified capability controls available when only the remote model catalogue fails", async () => {
  vi.mocked(listVideoModels).mockRejectedValue(new Error("401 Unauthorized"));
  const project: Project = {
    id: "test", name: "项目", description: "", storyStyle: "通用短剧", artStyle: "电影写实",
    aspectRatio: "16:9", imageModel: "gpt-image-2", imageQuality: "high", videoModel: "grok-imagine-video", videoResolution: "720p",
  };
  render(<ProjectSettingsPage open project={project} onClose={vi.fn()} />);

  await waitFor(() => expect(screen.getByText(/Grok Imagine Video 支持/)).toBeTruthy());
  expect((screen.getByLabelText("默认视频分辨率") as HTMLSelectElement).disabled).toBe(false);
  expect(screen.getByRole("status").textContent).toContain("模型目录");
});

it("does not prompt confirmation when closing after automatic resolution alignment on mount", async () => {
  const onClose = vi.fn();
  const project: Project = {
    id: "test", name: "项目", description: "", storyStyle: "通用短剧", artStyle: "电影写实",
    aspectRatio: "16:9", imageModel: "gpt-image-2", imageQuality: "high", videoModel: "minimax-h3", videoResolution: "720p",
  };
  render(<ProjectSettingsPage open project={project} onClose={onClose} />);
  await waitFor(() => expect(screen.getByRole("status").textContent).toContain("已切换为 2K"));

  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  expect(confirmAction).not.toHaveBeenCalled();
  expect(onClose).toHaveBeenCalledOnce();
});
