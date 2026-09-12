// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import StatusBar, { GITHUB_REPO_URL, ZZONE_INVITE_URL } from "./StatusBar";

const opener = vi.hoisted(() => ({ openPath: vi.fn(), openUrl: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => opener);
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
