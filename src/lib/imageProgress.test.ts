import { describe, expect, it, vi } from "vitest";

const listeners = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    listeners.set(name, handler);
    return () => listeners.delete(name);
  }),
}));

import {
  appendRunLog,
  formatBytes,
  listenImageProgress,
  progressButtonLabel,
  progressLogLevel,
  progressLogText,
  progressPercent,
  type ImageProgressEvent,
} from "./imageProgress";

describe("image progress helpers", () => {
  it("computes a percent only when the transfer size is known", () => {
    expect(progressPercent({ stage: "receiving", message: "", receivedBytes: 512, totalBytes: 2048 })).toBe(25);
    expect(progressPercent({ stage: "receiving", message: "", receivedBytes: 512 })).toBeNull();
    expect(progressPercent({ stage: "waiting", message: "" })).toBeNull();
    expect(progressPercent(null)).toBeNull();
  });

  it("labels the button for every backend step", () => {
    const at = (event: ImageProgressEvent | null, elapsedMs = 37_000, count = 1) => progressButtonLabel(event, elapsedMs, count);
    expect(at(null)).toBe("生成中… 37s");
    expect(at(null, 37_000, 3)).toBe("生成中…（3 张）37s");
    expect(at({ stage: "waiting", message: "" }, 125_000)).toBe("等待模型生成… 2m05s");
    expect(at({ stage: "receiving", message: "", receivedBytes: 300, totalBytes: 1000 })).toBe("接收结果 30%");
    expect(at({ stage: "receiving", message: "", receivedBytes: 3 * 1024 * 1024 })).toBe("接收结果 3.0 MB");
    expect(at({ stage: "downloading", message: "", receivedBytes: 1, totalBytes: 2, index: 2, count: 4 })).toBe("下载图片 2/4 50%");
    expect(at({ stage: "downloading", message: "", index: 1, count: 1 })).toBe("下载图片… 37s");
    expect(at({ stage: "retrying", message: "" })).toBe("网络中断，重试中… 37s");
    expect(at({ stage: "saving", message: "" })).toBe("保存图片…");
  });

  it("adds the received size to interrupted-download log lines", () => {
    const retry: ImageProgressEvent = { stage: "retrying", message: "第 1/1 张下载中断：超时；1 秒后续传（第 2/4 次）", receivedBytes: 2048 };
    expect(progressLogText(retry)).toBe("第 1/1 张下载中断：超时；1 秒后续传（第 2/4 次）（已收到 2 KB）");
    expect(progressLogLevel(retry)).toBe("warn");
    expect(progressLogLevel({ stage: "partial", message: "" })).toBe("warn");
    expect(progressLogLevel({ stage: "saving", message: "" })).toBe("info");
    expect(formatBytes(900)).toBe("900 B");
  });

  it("keeps one log line per step while byte counts repeat it", () => {
    let log = appendRunLog([], { offsetMs: 0, level: "info", text: "下载第 1/1 张图片" });
    log = appendRunLog(log, { offsetMs: 250, level: "info", text: "下载第 1/1 张图片" });
    expect(log).toHaveLength(1);
    log = appendRunLog(log, { offsetMs: 900, level: "warn", text: "下载中断" });
    log = appendRunLog(log, { offsetMs: 2000, level: "info", text: "下载第 1/1 张图片" });
    expect(log.map((entry) => entry.text)).toEqual(["下载第 1/1 张图片", "下载中断", "下载第 1/1 张图片"]);
  });

  it("delivers only the events of the subscribed run", async () => {
    const received: ImageProgressEvent[] = [];
    const unlisten = await listenImageProgress("task_a", (event) => received.push(event));
    const emit = listeners.get("image://progress")!;
    emit({ payload: { progressId: "task_b", stage: "waiting", message: "other run" } });
    emit({ payload: { progressId: "task_a", stage: "waiting", message: "this run" } });
    expect(received.map((event) => event.message)).toEqual(["this run"]);
    unlisten();
    expect(listeners.has("image://progress")).toBe(false);
  });
});
