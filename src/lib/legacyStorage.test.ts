// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./desktop", () => ({ isDesktop: vi.fn(() => true), invoke: vi.fn() }));
vi.mock("./logger", () => ({ logEvent: vi.fn() }));

import { invoke, isDesktop } from "./desktop";
import { restoreLegacyLocalStorage } from "./legacyStorage";

describe("restoreLegacyLocalStorage", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.mocked(isDesktop).mockReturnValue(true);
    vi.mocked(invoke).mockReset();
  });

  it("writes old entries without overwriting newer values", async () => {
    localStorage.setItem("kept", "new");
    vi.mocked(invoke).mockResolvedValue({ kept: "old", "video-md:last-workflow:p1": "w1" });
    await restoreLegacyLocalStorage();
    expect(invoke).toHaveBeenCalledWith("legacy_local_storage_take");
    expect(localStorage.getItem("kept")).toBe("new");
    expect(localStorage.getItem("video-md:last-workflow:p1")).toBe("w1");
  });

  it("never blocks startup when the backend fails", async () => {
    vi.mocked(invoke).mockRejectedValue("boom");
    await expect(restoreLegacyLocalStorage()).resolves.toBeUndefined();
  });

  it("does nothing outside the desktop shell", async () => {
    vi.mocked(isDesktop).mockReturnValue(false);
    await restoreLegacyLocalStorage();
    expect(invoke).not.toHaveBeenCalled();
  });
});
