import { beforeEach, describe, expect, it, vi } from "vitest";

const { loggedInvoke } = vi.hoisted(() => ({ loggedInvoke: vi.fn() }));

vi.mock("./logger", () => ({ loggedInvoke, logEvent: vi.fn() }));

import { getDb } from "./db";

describe("database client", () => {
  beforeEach(() => {
    loggedInvoke.mockReset();
  });

  it("does not cache a failed data_dir lookup", async () => {
    loggedInvoke.mockRejectedValueOnce(new Error("ipc unavailable"));
    await expect(getDb()).rejects.toThrow("ipc unavailable");

    // The next attempt must retry instead of reusing the rejected promise.
    loggedInvoke.mockResolvedValueOnce("C:/data").mockResolvedValue({ rowsAffected: 0, lastInsertId: 1 });
    const db = await getDb();
    await expect(db.select("SELECT 1")).resolves.toEqual({ rowsAffected: 0, lastInsertId: 1 });
    expect(loggedInvoke).toHaveBeenCalledWith("data_dir");
  });
});
