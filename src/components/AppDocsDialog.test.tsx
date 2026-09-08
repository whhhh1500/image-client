// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import AppDocsDialog from "./AppDocsDialog";

afterEach(cleanup);

it("shows the guide and closes from the close button", () => {
  const onClose = vi.fn();
  render(<AppDocsDialog view="guide" onClose={onClose} />);

  expect(screen.getByRole("dialog", { name: "使用文档" })).toBeTruthy();
  expect(screen.getByText("快速开始")).toBeTruthy();
  expect(screen.getByText("小说漫画")).toBeTruthy();
  expect(screen.getByText("AI 优化与版本")).toBeTruthy();
  expect(screen.getByText(/按生产依赖直接保存当前及已有下游文字产物的新版本/)).toBeTruthy();
  expect(screen.getByText(/不会自动重新生成视频/)).toBeTruthy();
  expect(screen.getByText("常见问题")).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "关闭使用文档" }));
  expect(onClose).toHaveBeenCalledOnce();
});

it("shows the changelog and closes with Escape", () => {
  const onClose = vi.fn();
  render(<AppDocsDialog view="changelog" onClose={onClose} />);

  expect(screen.getByRole("dialog", { name: "更新日志" })).toBeTruthy();
  const versionSection = (version: string) => {
    const section = screen.getByText(version).closest("section");
    expect(section, `${version} section`).toBeTruthy();
    return within(section!);
  };
  const v024 = versionSection("v0.2.4");
  expect(v024.getByText("2026-09-09")).toBeTruthy();
  expect(v024.getByText(/不再重复生成并计费已成功的镜头/)).toBeTruthy();
  expect(v024.getByText(/导入大文件时不再卡住整个应用/)).toBeTruthy();

  const v023 = versionSection("v0.2.3");
  expect(v023.getByText("2026-09-08")).toBeTruthy();
  expect(v023.getByText(/短剧采用带修订号的章节快照/)).toBeTruthy();
  expect(v023.getByText(/实际提交的完整 Prompt 快照/)).toBeTruthy();
  expect(v023.getByText(/Windows x64 portable\.exe、macOS arm64 app\.tar\.gz 和 Linux x64 AppImage/)).toBeTruthy();

  const v022 = versionSection("v0.2.2");
  expect(v022.getByText("2026-09-07")).toBeTruthy();
  expect(v022.getByText(/本地视频保留创作参考与来源信息/)).toBeTruthy();
  const v021 = versionSection("v0.2.1");
  expect(v021.getByText("2026-09-07")).toBeTruthy();
  expect(v021.getByText(/作品级多图画风参考/)).toBeTruthy();
  const v020 = versionSection("v0.2.0");
  expect(v020.getByText("2026-09-07")).toBeTruthy();
  expect(v020.getByText(/小说漫画 AI 优化会按作品设定/)).toBeTruthy();
  expect(v020.getByText(/短剧视频 AI 优化会按规划/)).toBeTruthy();
  const v010 = versionSection("v0.1.0");
  expect(v010.getByText("2026-09-06")).toBeTruthy();

  fireEvent.keyDown(window, { key: "Escape" });
  expect(onClose).toHaveBeenCalledOnce();
});

it("closes only when the backdrop itself is clicked", () => {
  const onClose = vi.fn();
  const { container } = render(<AppDocsDialog view="guide" onClose={onClose} />);
  const dialog = screen.getByRole("dialog", { name: "使用文档" });

  fireEvent.click(dialog);
  expect(onClose).not.toHaveBeenCalled();

  fireEvent.click(container.firstElementChild as Element);
  expect(onClose).toHaveBeenCalledOnce();
});
