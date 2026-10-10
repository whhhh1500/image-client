// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { confirmDialog, isDesktop } from "./desktop";
import { confirmAction } from "./confirm";
import { logEvent } from "./logger";

vi.mock("./desktop", () => ({ isDesktop: vi.fn(), confirmDialog: vi.fn() }));
vi.mock("./logger", () => ({ logEvent: vi.fn() }));

beforeEach(() => { vi.mocked(isDesktop).mockReturnValue(true); });
afterEach(() => { vi.restoreAllMocks(); vi.resetAllMocks(); });

describe("confirmAction", () => {
  it("shows the native confirm dialog with the message", async () => {
    vi.mocked(confirmDialog).mockResolvedValue(true);
    await expect(confirmAction("继续？")).resolves.toBe(true);
    expect(confirmDialog).toHaveBeenCalledWith("继续？", expect.objectContaining({ title: "确认操作", okLabel: "确定", cancelLabel: "取消" }));
  });

  it("returns false when the native dialog is cancelled", async () => {
    vi.mocked(confirmDialog).mockResolvedValue(false);
    await expect(confirmAction("继续？")).resolves.toBe(false);
  });

  it("blocks the action and records a dialog failure", async () => {
    vi.mocked(confirmDialog).mockRejectedValue(new Error("not allowed by ACL"));
    await expect(confirmAction("继续？")).resolves.toBe(false);
    expect(logEvent).toHaveBeenCalledWith("error", "dialog.confirm_failed", expect.any(Object));
  });

  it("preserves browser preview confirmation", async () => {
    vi.mocked(isDesktop).mockReturnValue(false);
    const browserConfirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    await expect(confirmAction("继续？")).resolves.toBe(false);
    expect(browserConfirm).toHaveBeenCalledWith("继续？");
    expect(confirmDialog).not.toHaveBeenCalled();
  });
});
