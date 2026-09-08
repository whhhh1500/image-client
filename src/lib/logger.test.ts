import { afterEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { initClientLogging, loggedInvoke, redactSensitiveText, summarizeForLog } from "./logger";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("frontend log sanitization", () => {
  it("redacts credentials and prompt bodies", () => {
    const value = summarizeForLog({ apiKey: "secret", prompt: "private", model: "safe" }) as Record<string, unknown>;
    expect(value.model).toBe("safe");
    expect(value.apiKey).toEqual({ redacted: true, chars: 6 });
    expect(value.prompt).toEqual({ redacted: true, chars: 7 });
  });

  it("keeps token usage metrics visible", () => {
    expect(summarizeForLog({ inputTokens: 12, outputTokens: 8, totalTokens: 20 })).toEqual({
      inputTokens: 12,
      outputTokens: 8,
      totalTokens: 20,
    });
  });

  it("redacts SQL bind values while keeping the statement", () => {
    expect(summarizeForLog({ query: "SELECT value FROM settings WHERE key = ?", bindValues: ["projects"] })).toEqual({
      query: "SELECT value FROM settings WHERE key = ?",
      bindValues: { redacted: true, items: 1 },
    });
  });

  it("summarizes binary data without copying it", () => {
    expect(summarizeForLog(new Uint8Array(128))).toEqual({ type: "Uint8Array", bytes: 128 });
  });

  it("measures binary payloads under redacted keys instead of enumerating them", () => {
    // Regression: `data` is a redacted key, and enumerating a 512 MiB payload
    // used to allocate one string per byte before the IPC call was made.
    expect(summarizeForLog({ kind: "video", ext: "mp4", data: new Uint8Array(1_000_000) })).toEqual({
      kind: "video",
      ext: "mp4",
      data: { type: "Uint8Array", bytes: 1_000_000 },
    });
    expect(summarizeForLog({ bindValues: new Uint8Array(64) })).toEqual({
      bindValues: { type: "Uint8Array", bytes: 64 },
    });
  });

  it("redacts credentials embedded in plain error text", () => {
    const safe = redactSensitiveText("HTTP 401 Authorization: Bearer TOPSECRET api_key=TOPSECRET password='pw' token=TOKEN access_token=ACCESS client_secret=SECRET SK-live-secret");
    expect(safe).toBe("HTTP 401 Authorization: Bearer <redacted> api_key=<redacted> password=<redacted> token=<redacted> access_token=<redacted> client_secret=<redacted> sk-<redacted>");
    expect(summarizeForLog({ error: "Authorization: Bearer TOPSECRET api_key=TOPSECRET" })).toEqual({ error: "Authorization: Bearer <redacted> api_key=<redacted>" });
  });

  it("sanitizes loggedInvoke error fields but rethrows the original error", async () => {
    const original = new Error("Authorization: Bearer TOPSECRET api_key=TOPSECRET");
    vi.useFakeTimers();
    vi.stubGlobal("window", {
      setTimeout: (callback: () => void, delay: number) => globalThis.setTimeout(callback, delay),
      clearTimeout: (timer: ReturnType<typeof setTimeout>) => globalThis.clearTimeout(timer),
    });
    vi.mocked(invoke).mockRejectedValueOnce(original).mockResolvedValue(undefined);
    await expect(loggedInvoke("comic_run_finish")).rejects.toBe(original);
    await vi.runAllTimersAsync();

    const clientLogsCall = vi.mocked(invoke).mock.calls.find(([command]) => command === "client_logs");
    expect(clientLogsCall).toBeDefined();
    const entries = (clientLogsCall?.[1] as { entries: Array<{ fields: Record<string, unknown> }> }).entries;
    expect(JSON.stringify(entries)).not.toContain("TOPSECRET");
    expect(entries.some(({ fields }) => typeof fields.error === "string" && fields.error.includes("Authorization: Bearer <redacted> api_key=<redacted>"))).toBe(true);
  });

  it("initializes telemetry once and releases every listener, observer, and timer", () => {
    const documentAdd = vi.fn();
    const documentRemove = vi.fn();
    const windowAdd = vi.fn();
    const windowRemove = vi.fn();
    const setTimeout = vi.fn(() => 11);
    const clearTimeout = vi.fn();
    const setInterval = vi.fn(() => 22);
    const clearInterval = vi.fn();
    const observer = { observe: vi.fn(), disconnect: vi.fn() };
    class TestPerformanceObserver {
      observe = observer.observe;
      disconnect = observer.disconnect;
      constructor(_callback: PerformanceObserverCallback) {}
    }

    vi.stubGlobal("navigator", { userAgent: "test", language: "zh-CN" });
    vi.stubGlobal("document", { addEventListener: documentAdd, removeEventListener: documentRemove, visibilityState: "visible" });
    vi.stubGlobal("PerformanceObserver", TestPerformanceObserver);
    vi.stubGlobal("window", {
      addEventListener: windowAdd,
      removeEventListener: windowRemove,
      setTimeout,
      clearTimeout,
      setInterval,
      clearInterval,
      innerWidth: 100,
      innerHeight: 200,
      devicePixelRatio: 1,
    });

    const firstCleanup = initClientLogging();
    const secondCleanup = initClientLogging();

    expect(secondCleanup).toBe(firstCleanup);
    expect(documentAdd).toHaveBeenCalledTimes(3);
    expect(windowAdd).toHaveBeenCalledTimes(2);
    expect(setInterval).toHaveBeenCalledTimes(1);
    expect(observer.observe).toHaveBeenCalledTimes(1);

    firstCleanup();
    firstCleanup();

    expect(documentRemove).toHaveBeenCalledTimes(3);
    expect(windowRemove).toHaveBeenCalledTimes(2);
    expect(clearInterval).toHaveBeenCalledWith(22);
    expect(observer.disconnect).toHaveBeenCalledTimes(1);
    expect(clearTimeout).toHaveBeenCalledWith(11);
  });

  it("does not reschedule a flush that completes after cleanup", async () => {
    vi.useFakeTimers();
    let resolveClientLogs: (() => void) | undefined;
    const clientLogsPending = new Promise<void>((resolve) => { resolveClientLogs = resolve; });
    vi.mocked(invoke).mockImplementation((command) => command === "client_logs" ? clientLogsPending as never : Promise.resolve(undefined) as never);

    const setTimeout = vi.fn((callback: () => void, delay: number) => globalThis.setTimeout(callback, delay));
    const clearTimeout = vi.fn((timer: ReturnType<typeof setTimeout>) => globalThis.clearTimeout(timer));
    const setInterval = vi.fn(() => 22);
    const clearInterval = vi.fn();
    vi.stubGlobal("navigator", { userAgent: "test", language: "zh-CN" });
    vi.stubGlobal("document", { addEventListener: vi.fn(), removeEventListener: vi.fn(), visibilityState: "visible" });
    vi.stubGlobal("PerformanceObserver", class {
      observe() {}
      disconnect() {}
      constructor(_callback: PerformanceObserverCallback) {}
    });
    vi.stubGlobal("window", {
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      setTimeout,
      clearTimeout,
      setInterval,
      clearInterval,
      innerWidth: 100,
      innerHeight: 200,
      devicePixelRatio: 1,
    });

    const cleanup = initClientLogging();
    vi.advanceTimersByTime(250);
    await Promise.resolve();
    expect(resolveClientLogs).toBeDefined();

    cleanup();
    resolveClientLogs?.();
    await Promise.resolve();
    await Promise.resolve();

    expect(setTimeout).toHaveBeenCalledTimes(1);
  });
});
