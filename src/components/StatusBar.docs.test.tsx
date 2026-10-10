// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import StatusBar, { GITHUB_REPO_URL, ZZONE_INVITE_URL } from "./StatusBar";

const opener = vi.hoisted(() => ({ openPath: vi.fn(), openUrl: vi.fn() }));
vi.mock("../lib/desktop", () => opener);
vi.mock("../lib/ipc", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/ipc")>();
  return { ...original, logsDir: vi.fn() };
});

afterEach(() => { cleanup(); vi.clearAllMocks(); });

it("opens the guide and changelog from the right side of the status bar", () => {
  const onOpenGuide = vi.fn();
  const onOpenChangelog = vi.fn();
  render(
    <StatusBar
      appInfo={null}
      dbReady={false}
      configStatus={null}
      providers={null}
      onOpenSettings={vi.fn()}
      onOpenGuide={onOpenGuide}
      onOpenChangelog={onOpenChangelog}
    />,
  );

  fireEvent.click(screen.getByRole("button", { name: "使用文档" }));
  fireEvent.click(screen.getByRole("button", { name: "更新日志" }));

  expect(onOpenGuide).toHaveBeenCalledOnce();
  expect(onOpenChangelog).toHaveBeenCalledOnce();
});

it("opens the active ZZone gateway invitation in the system browser", () => {
  opener.openUrl.mockResolvedValue(undefined);
  render(
    <StatusBar
      appInfo={null}
      dbReady
      configStatus={null}
      providers={[{ id: "zzone", name: "ZZone 网关", active: true, capabilities: ["image"] }]}
      onOpenSettings={vi.fn()}
      onOpenGuide={vi.fn()}
      onOpenChangelog={vi.fn()}
    />,
  );

  fireEvent.click(screen.getByRole("button", { name: "ZZone 网关" }));
  expect(opener.openUrl).toHaveBeenCalledWith(ZZONE_INVITE_URL);
});

it("opens the GitHub repository in the system browser", () => {
  opener.openUrl.mockResolvedValue(undefined);
  render(
    <StatusBar
      appInfo={null}
      dbReady
      configStatus={null}
      providers={null}
      onOpenSettings={vi.fn()}
      onOpenGuide={vi.fn()}
      onOpenChangelog={vi.fn()}
    />,
  );

  fireEvent.click(screen.getByRole("button", { name: "GitHub 仓库" }));
  expect(opener.openUrl).toHaveBeenCalledWith(GITHUB_REPO_URL);
});

it("labels the configured default image model and a failed startup load", () => {
  const props = { appInfo: null, providers: null, onOpenSettings: vi.fn(), onOpenGuide: vi.fn(), onOpenChangelog: vi.fn() };
  const status = { imageReady: true, videoReady: false, llmReady: false, imageModel: "gpt-image-2", videoModel: "", llmModel: "", outputDir: "", source: "db" as const };
  const { rerender } = render(<StatusBar {...props} dbReady configStatus={status} />);

  // The default from 设置 › 接口配置, named so it is not mistaken for the
  // model the 图像生成 toolbar uses for the next run.
  expect(screen.getByText("默认图像模型 · gpt-image-2")).toBeTruthy();
  expect(screen.getByText("就绪")).toBeTruthy();

  rerender(<StatusBar {...props} dbReady={false} startupFailed configStatus={{ ...status, imageReady: false }} />);
  expect(screen.getByText("默认图像模型 · 未配置")).toBeTruthy();
  // A failed load no longer looks like a startup that is still running.
  expect(screen.getByText("加载失败")).toBeTruthy();
  expect(screen.queryByText("初始化中…")).toBeNull();
});
