// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { mediaHostingGet, mediaHostingSave } from "../lib/ipc";
import MediaHostingDialog from "./MediaHostingDialog";

vi.mock("../lib/ipc", () => ({ mediaHostingGet: vi.fn(), mediaHostingSave: vi.fn() }));

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
  await screen.findByDisplayValue("https://media.example/upload");

  fireEvent.click(screen.getByRole("button", { name: "保存配置" }));

  expect((await screen.findByRole("alert")).textContent).toBe("Endpoint 必须是 HTTPS 地址");
  expect(screen.getAllByRole("alert")).toHaveLength(1);
  expect(onClose).not.toHaveBeenCalled();
});
