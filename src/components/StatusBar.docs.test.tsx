// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import StatusBar from "./StatusBar";

vi.mock("@tauri-apps/plugin-opener", () => ({ openPath: vi.fn() }));
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
