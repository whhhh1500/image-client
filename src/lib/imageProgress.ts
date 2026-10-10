import { listen, type UnlistenFn } from "./desktop";

/** Mirrors `gateway::ImageProgressEvent` (src-tauri/src/gateway.rs). */
export type ImageProgressStage = "waiting" | "receiving" | "downloading" | "retrying" | "partial" | "saving";

export interface ImageProgressEvent {
  stage: ImageProgressStage;
  message: string;
  receivedBytes?: number;
  totalBytes?: number;
  /** 1-based image number while downloading. */
  index?: number;
  count?: number;
}

export type ImageRunLogLevel = "info" | "warn" | "error" | "success";

export interface ImageRunLogEntry {
  /** Milliseconds since the run started. */
  offsetMs: number;
  level: ImageRunLogLevel;
  text: string;
}

/** Same name as `IMAGE_PROGRESS_EVENT` in src-tauri/src/commands.rs. */
const IMAGE_PROGRESS_EVENT = "image://progress";

/** Subscribe to one run's steps; events of other runs are ignored. */
export function listenImageProgress(progressId: string, onEvent: (event: ImageProgressEvent) => void): Promise<UnlistenFn> {
  return listen<ImageProgressEvent & { progressId: string }>(IMAGE_PROGRESS_EVENT, ({ payload }) => {
    if (payload.progressId === progressId) onEvent(payload);
  });
}

export function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

/** Whole percent of a transfer, or null when the size is unknown. */
export function progressPercent(event: ImageProgressEvent | null): number | null {
  if (!event?.totalBytes || event.receivedBytes === undefined) return null;
  return Math.min(100, Math.floor((event.receivedBytes / event.totalBytes) * 100));
}

function elapsedLabel(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m${String(seconds % 60).padStart(2, "0")}s`;
}

/** Short text for the generate button while a run is in flight. */
export function progressButtonLabel(event: ImageProgressEvent | null, elapsedMs: number, count: number): string {
  const elapsed = elapsedLabel(elapsedMs);
  const percent = progressPercent(event);
  const amount = percent !== null ? `${percent}%` : event?.receivedBytes ? formatBytes(event.receivedBytes) : null;
  switch (event?.stage) {
    case "receiving":
      return amount ? `接收结果 ${amount}` : `接收结果… ${elapsed}`;
    case "downloading": {
      const which = event.count && event.count > 1 ? ` ${event.index}/${event.count}` : "";
      return amount ? `下载图片${which} ${amount}` : `下载图片${which}… ${elapsed}`;
    }
    case "retrying":
      return `网络中断，重试中… ${elapsed}`;
    case "saving":
      return "保存图片…";
    case "waiting":
      return `等待模型生成… ${elapsed}`;
    default:
      return count > 1 ? `生成中…（${count} 张）${elapsed}` : `生成中… ${elapsed}`;
  }
}

/** Log line for a step, with the byte count when a transfer was interrupted. */
export function progressLogText(event: ImageProgressEvent): string {
  if (event.stage === "retrying" && event.receivedBytes) return `${event.message}（已收到 ${formatBytes(event.receivedBytes)}）`;
  return event.message;
}

export function progressLogLevel(event: ImageProgressEvent): ImageRunLogLevel {
  return event.stage === "retrying" || event.stage === "partial" ? "warn" : "info";
}

/**
 * Byte updates repeat a step's message; only a new message becomes a log line,
 * so a long download is one entry rather than hundreds.
 */
export function appendRunLog(log: readonly ImageRunLogEntry[], entry: ImageRunLogEntry): ImageRunLogEntry[] {
  const last = log[log.length - 1];
  if (last && last.text === entry.text && last.level === entry.level) return log as ImageRunLogEntry[];
  return [...log, entry];
}
