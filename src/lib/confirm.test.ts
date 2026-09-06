// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { isTauri } from "@tauri-apps/api/core";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { confirmAction } from "./confirm";
import { logEvent } from "./logger";

vi.mock("@tauri-apps/api/core", () => ({ isTauri: vi.fn() }));
vi.mock("./logger", () => ({ logEvent: vi.fn() }));

const invoke = vi.fn();
beforeEach(() => { vi.mocked(isTauri).mockReturnValue(true); mockIPC(invoke); });
afterEach(() => { clearMocks(); vi.restoreAllMocks(); vi.resetAllMocks(); });

describe("confirmAction", () => {
  it("uses the message command supported by the installed dialog plugin", async () => {
    vi.mocked(invoke).mockResolvedValue("确定");
    await expect(confirmAction("继续？")).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith("plugin:dialog|message", expect.objectContaining({ message: "继续？" }));
  });

  it("returns false when the native dialog is cancelled", async () => {
    vi.mocked(invoke).mockResolvedValue("取消");
    await expect(confirmAction("继续？")).resolves.toBe(false);
  });

  it("blocks the action and records a dialog failure", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("not allowed by ACL"));
    await expect(confirmAction("继续？")).resolves.toBe(false);
    expect(logEvent).toHaveBeenCalledWith("error", "dialog.confirm_failed", expect.any(Object));
  });

  it("preserves browser preview confirmation", async () => {
    vi.mocked(isTauri).mockReturnValue(false);
    const browserConfirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    await expect(confirmAction("继续？")).resolves.toBe(false);
    expect(browserConfirm).toHaveBeenCalledWith("继续？");
    expect(invoke).not.toHaveBeenCalled();
  });
});
