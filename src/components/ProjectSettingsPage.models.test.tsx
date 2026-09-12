// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { confirmAction } from "../lib/confirm";
import { fetchModels, listVideoModelCapabilities, listVideoModels } from "../lib/ipc";
import ProjectSettingsPage from "./ProjectSettingsPage";
import type { Project } from "../store/useProjectStore";

vi.mock("../lib/confirm", () => ({ confirmAction: vi.fn().mockResolvedValue(true) }));
vi.mock("../lib/ipc", () => ({
  fetchModels: vi.fn(),
  listVideoModels: vi.fn(),
  listVideoModelCapabilities: vi.fn(),
}));

const project = (): Project => ({
  id: "p1",
  name: "项目",
  description: "",
  storyStyle: "通用短剧",
  artStyle: "电影写实",
  aspectRatio: "16:9",
  imageModel: "gpt-image-2",
  imageQuality: "high",
  videoModel: "kling-video-v3",
  videoResolution: "720p",
});

beforeEach(() => {
  vi.mocked(listVideoModels).mockResolvedValue(["kling-video-v3"]);
  vi.mocked(listVideoModelCapabilities).mockResolvedValue([]);
});

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

describe("ProjectSettingsPage model catalog", () => {
  it("fetches the image catalog and accepts a model that is in no list", async () => {
    vi.mocked(fetchModels).mockResolvedValue(["brand-new-image"]);
    render(<ProjectSettingsPage open project={project()} onClose={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "获取模型（默认图像模型）" }));

    await waitFor(() => {
      expect(vi.mocked(fetchModels)).toHaveBeenCalledWith({ url: "", key: "", kind: "image" });
    });
    await waitFor(() => expect(screen.getByText(/已获取 1 个模型/)).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "展开默认图像模型列表" }));
    expect(screen.queryByRole("option", { name: "gpt-image-2" })).toBeNull();
    fireEvent.click(screen.getByRole("option", { name: "brand-new-image" }));
    expect((screen.getByRole("combobox", { name: "默认图像模型" }) as HTMLInputElement).value)
      .toBe("brand-new-image");

    fireEvent.change(screen.getByRole("combobox", { name: "默认图像模型" }), {
      target: { value: "自填图像模型" },
    });
    // The hand-typed value is a real draft edit, so closing now asks first.
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(confirmAction).toHaveBeenCalledOnce();
  });

  it("fetches the video catalog and reports a failure without dropping the current model", async () => {
    vi.mocked(fetchModels).mockRejectedValue("请先填写 API Key");
    render(<ProjectSettingsPage open project={project()} onClose={vi.fn()} />);

    await waitFor(() => {
      expect((screen.getByRole("combobox", { name: "默认视频模型" }) as HTMLInputElement).value)
        .toBe("kling-video-v3");
    });

    fireEvent.click(screen.getByRole("button", { name: "获取模型（默认视频模型）" }));

    await waitFor(() => {
      expect(vi.mocked(fetchModels)).toHaveBeenCalledWith({ url: "", key: "", kind: "video" });
    });
    expect(await screen.findByText("获取模型失败：请先填写 API Key")).toBeTruthy();
    expect((screen.getByRole("combobox", { name: "默认视频模型" }) as HTMLInputElement).value)
      .toBe("kling-video-v3");
  });
});
