// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { confirmAction } from "../lib/confirm";
import { mediaHostingGet, mediaHostingSave } from "../lib/ipc";
import MediaHostingDialog from "./MediaHostingDialog";

vi.mock("../lib/ipc", () => ({ mediaHostingGet: vi.fn(), mediaHostingSave: vi.fn() }));
vi.mock("../lib/confirm", () => ({ confirmAction: vi.fn() }));

beforeEach(() => {
  vi.mocked(mediaHostingGet).mockResolvedValue({
    endpoint: "https://media.example/upload", fileField: "file", urlField: "url",
    authMode: "bearer", hasToken: false, configured: true,
  });
});
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("closes on Escape", async () => {
  const onClose = vi.fn();
  render(<MediaHostingDialog open onClose={onClose} />);
  await screen.findByDisplayValue("https://media.example/upload");

  fireEvent.keyDown(window, { key: "Escape" });
  expect(onClose).toHaveBeenCalledOnce();
});

it("reports a failed save once, without an Error: prefix, and stays open", async () => {
  vi.mocked(mediaHostingSave).mockRejectedValue(new Error("Endpoint 必须是 HTTPS 地址"));
  const onClose = vi.fn();
  render(<MediaHostingDialog open onClose={onClose} />);
  fireEvent.change(await screen.findByDisplayValue("https://media.example/upload"), { target: { value: "http://media.example/upload" } });

  fireEvent.click(screen.getByRole("button", { name: "保存配置" }));

  expect((await screen.findByRole("alert")).textContent).toBe("Endpoint 必须是 HTTPS 地址");
  expect(screen.getAllByRole("alert")).toHaveLength(1);
  expect(onClose).not.toHaveBeenCalled();
});

const saveButton = () => screen.getByRole("button", { name: "保存配置" }) as HTMLButtonElement;

it("keeps the form locked until the current setup is read, and saves only changes", async () => {
  let finish!: () => void;
  vi.mocked(mediaHostingGet).mockReturnValue(new Promise((resolve) => {
    finish = () => resolve({ endpoint: "https://media.example/upload", fileField: "file", urlField: "url", authMode: "bearer", hasToken: false, configured: true });
  }));
  render(<MediaHostingDialog open onClose={vi.fn()} />);
  expect(screen.getByLabelText("上传地址").matches(":disabled")).toBe(true);
  expect(screen.getByText("正在读取当前配置…")).toBeTruthy();
  expect(saveButton().disabled).toBe(true);

  await act(async () => finish());
  expect(screen.getByLabelText("上传地址").matches(":disabled")).toBe(false);
  expect(saveButton().disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("媒体托管令牌"), { target: { value: "secret" } });
  expect(saveButton().disabled).toBe(false);
});

it("never saves over the stored setup when reading it failed", async () => {
  vi.mocked(mediaHostingGet).mockRejectedValue(new Error("database is locked"));
  render(<MediaHostingDialog open onClose={vi.fn()} />);
  expect((await screen.findByRole("alert")).textContent).toBe("读取媒体托管配置失败：database is locked");
  expect(screen.getByLabelText("上传地址").matches(":disabled")).toBe(true);
  expect(saveButton().disabled).toBe(true);
});

it("asks before Escape drops unsaved changes", async () => {
  const onClose = vi.fn();
  vi.mocked(confirmAction).mockResolvedValue(false);
  render(<MediaHostingDialog open onClose={onClose} />);
  fireEvent.change(await screen.findByDisplayValue("https://media.example/upload"), { target: { value: "https://other.example/upload" } });

  fireEvent.keyDown(window, { key: "Escape" });
  await act(async () => {});
  expect(confirmAction).toHaveBeenCalledWith("媒体托管配置有未保存修改，确定关闭吗？");
  expect(onClose).not.toHaveBeenCalled();
});
