// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  confirmDialog,
  fileSrc,
  invoke,
  isDesktop,
  listen,
  openDialog,
  openPath,
  openUrl,
  revealItemInDir,
} from "./desktop";

const bridge = () => {
  const Invoke = vi.fn().mockResolvedValue("ok");
  window.go = { app: { Bridge: { Invoke } } };
  return Invoke;
};

afterEach(() => {
  delete window.go;
  delete window.runtime;
});

describe("desktop adapter", () => {
  it("forwards invoke to the Go bridge with empty default args", async () => {
    const Invoke = bridge();
    await expect(invoke("ping")).resolves.toBe("ok");
    expect(Invoke).toHaveBeenCalledWith("ping", {});
  });

  it("forwards explicit args to the Go bridge", async () => {
    const Invoke = bridge();
    await invoke("save", { id: 1 });
    expect(Invoke).toHaveBeenCalledWith("save", { id: 1 });
  });

  it("rejects with the backend message string, without an Error wrapper", async () => {
    const Invoke = bridge();
    Invoke.mockRejectedValueOnce(new Error("请先填写 API Key"));
    await expect(invoke("fetch_models")).rejects.toBe("请先填写 API Key");
    Invoke.mockRejectedValueOnce("plain message");
    await expect(invoke("fetch_models")).rejects.toBe("plain message");
  });

  it("rejects invoke when the desktop runtime is missing", async () => {
    expect(isDesktop()).toBe(false);
    await expect(invoke("ping")).rejects.toThrow("桌面运行时不可用");
  });

  it("wraps event payloads and returns the unsubscribe function", async () => {
    const off = vi.fn();
    const EventsOn = vi.fn().mockReturnValue(off);
    window.runtime = { EventsOn };
    const handler = vi.fn();
    const unlisten = await listen<{ n: number }>("image://progress", handler);
    expect(EventsOn).toHaveBeenCalledWith("image://progress", expect.any(Function));
    const callback = EventsOn.mock.calls[0][1];
    callback({ n: 7 });
    expect(handler).toHaveBeenCalledWith({ payload: { n: 7 } });
    expect(unlisten).toBe(off);
  });

  it("rejects listen when the event runtime is missing", async () => {
    await expect(listen("x", vi.fn())).rejects.toThrow("桌面运行时不可用");
  });

  it("encodes Windows paths into a file URL", () => {
    expect(fileSrc("C:\\Users\\a b\\x.png")).toBe("/__file/" + encodeURIComponent("C:\\Users\\a b\\x.png"));
    expect(fileSrc("C:\\Users\\a b\\x.png")).toBe("/__file/C%3A%5CUsers%5Ca%20b%5Cx.png");
  });

  it("opens the native file dialog through invoke", async () => {
    const Invoke = bridge();
    const options = { multiple: true, filters: [{ name: "图片", extensions: ["png"] }] };
    await openDialog(options);
    expect(Invoke).toHaveBeenCalledWith("dialog_open", { options });
  });

  it("opens the native confirm dialog through invoke", async () => {
    const Invoke = bridge();
    await confirmDialog("继续？", { title: "确认操作", okLabel: "确定", cancelLabel: "取消" });
    expect(Invoke).toHaveBeenCalledWith("dialog_confirm", { message: "继续？", title: "确认操作", okLabel: "确定", cancelLabel: "取消" });
  });

  it("reveals, opens and launches items through invoke", async () => {
    const Invoke = bridge();
    await revealItemInDir("C:\\a\\x.png");
    await openPath("C:\\a\\x.png");
    await openUrl("https://example.com");
    expect(Invoke).toHaveBeenNthCalledWith(1, "reveal_item_in_dir", { path: "C:\\a\\x.png" });
    expect(Invoke).toHaveBeenNthCalledWith(2, "open_path", { path: "C:\\a\\x.png" });
    expect(Invoke).toHaveBeenNthCalledWith(3, "open_url", { url: "https://example.com" });
  });
});
