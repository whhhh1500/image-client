// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { confirmAction } from "../lib/confirm";
import PromptManager from "./PromptManager";

vi.mock("../lib/confirm", () => ({ confirmAction: vi.fn() }));
vi.mock("../lib/db", () => ({ dbExecute: vi.fn(), dbSelect: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.mocked(confirmAction).mockReset();
});

describe("PromptManager keyboard", () => {
  it("starts in the prompt editor and sends Escape through the unsaved-change guard", async () => {
    const onClose = vi.fn();
    render(<PromptManager open onClose={onClose} initialAgent="director" />);
    const editor = screen.getByRole("textbox", { name: "Prompt 内容" });
    expect(screen.getByRole("dialog", { name: "Prompt 管理" })).toBeTruthy();
    expect(document.activeElement).toBe(editor);

    fireEvent.change(editor, { target: { value: "新的导演 Prompt" } });
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(confirmAction).toHaveBeenCalledWith("Prompt 有未保存修改，确定关闭吗？"));
    expect(onClose).not.toHaveBeenCalled();

    vi.mocked(confirmAction).mockResolvedValueOnce(true);
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });
});
