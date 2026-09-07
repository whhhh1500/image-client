// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
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
  expect(screen.getByText("v0.2.0")).toBeTruthy();
  expect(screen.getByText("2026-09-07")).toBeTruthy();
  expect(screen.getByText(/小说漫画 AI 优化会按作品设定/)).toBeTruthy();
  expect(screen.getByText(/短剧视频 AI 优化会按规划/)).toBeTruthy();
  expect(screen.getByText("v0.1.0")).toBeTruthy();
  expect(screen.getByText("2026-09-06")).toBeTruthy();

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
