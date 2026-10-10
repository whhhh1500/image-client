// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { confirmAction } from "../lib/confirm";
import { optimizeImagePrompt } from "../lib/optimizePrompt";
import OptimizePromptDialog from "./OptimizePromptDialog";

vi.mock("../lib/confirm", () => ({ confirmAction: vi.fn() }));
vi.mock("../lib/optimizePrompt", () => ({ optimizeImagePrompt: vi.fn() }));

const getPrompt = () => "雨夜街道";
const backdrop = () => screen.getByRole("dialog", { name: "AI 优化成中文提示词" }).parentElement!;
const pressEscape = () => fireEvent.keyDown(window, { key: "Escape" });

function renderDialog(extra: { onSaveAsTemplate?: (prompt: string) => Promise<void> } = {}) {
  const onClose = vi.fn();
  render(<OptimizePromptDialog open getPrompt={getPrompt} onClose={onClose} onAdopt={vi.fn()} {...extra} />);
  return onClose;
}

async function optimize() {
  vi.mocked(optimizeImagePrompt).mockResolvedValueOnce("优化后的雨夜街道");
  fireEvent.click(screen.getByRole("button", { name: "开始优化" }));
  await screen.findByDisplayValue("优化后的雨夜街道");
}

afterEach(() => {
  cleanup();
  vi.mocked(confirmAction).mockReset();
  vi.mocked(optimizeImagePrompt).mockReset();
});

describe("OptimizePromptDialog closing", () => {
  it("closes straight away from the backdrop when there is nothing to lose", () => {
    const onClose = renderDialog();
    expect(document.activeElement).toBe(screen.getByPlaceholderText(/补充要求/));
    fireEvent.click(backdrop());
    expect(confirmAction).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("asks before the backdrop or Escape drops an unapplied result", async () => {
    const onClose = renderDialog();
    await optimize();

    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    pressEscape();
    await waitFor(() => expect(confirmAction).toHaveBeenCalledWith("优化结果尚未采用，确定关闭吗？"));
    expect(onClose).not.toHaveBeenCalled();

    vi.mocked(confirmAction).mockResolvedValueOnce(true);
    fireEvent.click(backdrop());
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(confirmAction).toHaveBeenCalledTimes(2);
  });

  it("asks while a request is running", async () => {
    vi.mocked(optimizeImagePrompt).mockReturnValueOnce(new Promise(() => {}));
    const onClose = renderDialog();
    fireEvent.click(screen.getByRole("button", { name: "开始优化" }));

    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    pressEscape();
    await waitFor(() => expect(confirmAction).toHaveBeenCalledWith("优化仍在进行，关闭后将丢弃结果，确定关闭吗？"));
    expect(onClose).not.toHaveBeenCalled();
  });

  it("closes without asking once the result is saved to the template", async () => {
    const save = vi.fn().mockResolvedValue(undefined);
    const onClose = renderDialog({ onSaveAsTemplate: save });
    await optimize();

    await act(async () => fireEvent.click(screen.getByRole("button", { name: "存回模板" })));
    expect(save).toHaveBeenCalledWith("优化后的雨夜街道");

    pressEscape();
    expect(confirmAction).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
