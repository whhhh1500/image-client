import { invoke } from "@tauri-apps/api/core";

export type LogLevel = "debug" | "info" | "warn" | "error";
type LogFields = Record<string, unknown>;
interface ClientLogEntry {
  level: LogLevel;
  event: string;
  fields: LogFields;
}

const queue: ClientLogEntry[] = [];
let flushing = false;
let flushTimer: number | undefined;
let backendRetryAt = 0;
let loggingLifecycleActive = true;
let loggingLifecycleGeneration = 0;
let activeClientLoggingCleanup: (() => void) | undefined;
const BATCH_SIZE = 50;
const FLUSH_INTERVAL_MS = 250;
const MAX_QUEUE_SIZE = 2_000;
const startedAt = performance.now();

const privateKey = (key: string) => {
  const normalized = key.replace(/[-_]/g, "").toLowerCase();
  if (["authorization", "password", "secret", "apikey", "accesskey", "privatekey"].some((value) => normalized.includes(value))) return true;
  if (["token", "accesstoken", "refreshtoken", "authtoken", "bearertoken"].includes(normalized)) return true;
  return ["prompt", "system", "content", "text", "user", "data", "input", "result", "response", "body", "database64", "bindvalues"].includes(normalized);
};

/** Redact credentials that can appear inside provider/RPC error text. */
export function redactSensitiveText(value: string): string {
  return value
    .replace(/\bBearer\s+[^\s"'`},]+/gi, "Bearer <redacted>")
    .replace(/\bsk-[A-Za-z0-9._-]+/gi, "sk-<redacted>")
    .replace(/((?:["']?(?:api[_-]?key|access[_-]?key|secret|password|authorization|token|access[_-]?token|refresh[_-]?token|auth[_-]?token|bearer[_-]?token)["']?\s*[:=]\s*))(?!Bearer\s)(?:"(?:[^"\\]|\\.)*"|'[^']*'|[^\s,;\]}]+)/gi, "$1<redacted>");
}

export function summarizeForLog(value: unknown, key = "", depth = 0): unknown {
  if (depth > 4) return "<max-depth>";
  if (privateKey(key)) {
    if (typeof value === "string") return { redacted: true, chars: value.length };
    if (Array.isArray(value)) return { redacted: true, items: value.length };
    if (value && typeof value === "object") return { redacted: true, keys: Object.keys(value).length };
    return "<redacted>";
  }
  if (value instanceof Uint8Array) return { type: "Uint8Array", bytes: value.byteLength };
  if (typeof value === "string") {
    const safe = redactSensitiveText(value);
    return safe.length > 256 ? `${safe.slice(0, 256)}…` : safe;
  }
  if (typeof value === "number" || typeof value === "boolean" || value == null) return value;
  if (Array.isArray(value)) return value.slice(0, 30).map((item) => summarizeForLog(item, "item", depth + 1));
  if (typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .slice(0, 50)
        .map(([childKey, child]) => [childKey, summarizeForLog(child, childKey, depth + 1)]),
    );
  }
  return String(value);
}

async function flush(generation = loggingLifecycleGeneration) {
  if (flushing) return;
  if (flushTimer !== undefined) {
    window.clearTimeout(flushTimer);
    flushTimer = undefined;
  }
  const retryDelay = backendRetryAt - Date.now();
  if (retryDelay > 0) {
    flushTimer = window.setTimeout(() => void flush(generation), retryDelay);
    return;
  }
  flushing = true;
  try {
    while (queue.length) {
      const entries = queue.splice(0, BATCH_SIZE);
      try {
        await invoke("client_logs", { entries });
      } catch {
        // Browser-only development has no Tauri backend. Logging must never break the UI.
        queue.unshift(...entries);
        if (queue.length > MAX_QUEUE_SIZE) queue.splice(MAX_QUEUE_SIZE);
        backendRetryAt = Date.now() + 5_000;
        break;
      }
    }
  } finally {
    flushing = false;
    if (queue.length && loggingLifecycleActive && generation === loggingLifecycleGeneration) scheduleFlush(generation);
  }
}

function scheduleFlush(generation = loggingLifecycleGeneration) {
  if (!loggingLifecycleActive) return;
  if (queue.length >= BATCH_SIZE) {
    void flush(generation);
  } else if (flushTimer === undefined) {
    flushTimer = window.setTimeout(() => void flush(generation), FLUSH_INTERVAL_MS);
  }
}

export function logEvent(level: LogLevel, event: string, fields: LogFields = {}) {
  if (queue.length >= MAX_QUEUE_SIZE) queue.splice(0, BATCH_SIZE);
  queue.push({ level, event, fields: summarizeForLog(fields) as LogFields });
  scheduleFlush();
}

export async function loggedInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const started = performance.now();
  logEvent("debug", "ipc.request.start", { command, args });
  try {
    const result = await invoke<T>(command, args);
    logEvent("info", "ipc.request.end", {
      command,
      durationMs: Math.round((performance.now() - started) * 100) / 100,
      status: "success",
      result: summarizeForLog(result, "result"),
    });
    return result;
  } catch (error) {
    logEvent("error", "ipc.request.end", {
      command,
      durationMs: Math.round((performance.now() - started) * 100) / 100,
      status: "error",
      error: String(error),
    });
    throw error;
  }
}

function controlInfo(target: Element) {
  const control = target.closest("button,a,select,input,textarea,[role='button']") as HTMLElement | null;
  if (!control) return null;
  const input = control as HTMLInputElement;
  return {
    tag: control.tagName.toLowerCase(),
    type: input.type || undefined,
    id: control.id || undefined,
    name: input.name || undefined,
    label: (control.getAttribute("aria-label") || control.getAttribute("title") || control.textContent || "")
      .trim()
      .replace(/\s+/g, " ")
      .slice(0, 100),
  };
}

export function initClientLogging(): () => void {
  if (activeClientLoggingCleanup) return activeClientLoggingCleanup;
  loggingLifecycleActive = true;
  const generation = ++loggingLifecycleGeneration;

  const onClick = (event: Event) => {
    const info = event.target instanceof Element ? controlInfo(event.target) : null;
    if (info) logEvent("info", "ui.click", info);
  };
  const onChange = (event: Event) => {
    const info = event.target instanceof Element ? controlInfo(event.target) : null;
    if (info) logEvent("info", "ui.change", info);
  };
  const onVisibilityChange = () => {
    logEvent("info", "frontend.visibility", { state: document.visibilityState });
    if (document.visibilityState === "hidden") void flush(generation);
  };
  const onPageHide = () => void flush(generation);
  const onLoad = () => {
    const nav = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming | undefined;
    logEvent("info", "performance.navigation", nav ? {
      domInteractiveMs: nav.domInteractive,
      domContentLoadedMs: nav.domContentLoadedEventEnd,
      loadMs: nav.loadEventEnd,
      transferBytes: nav.transferSize,
      decodedBytes: nav.decodedBodySize,
    } : {});
  };

  document.addEventListener("click", onClick, true);
  document.addEventListener("change", onChange, true);
  document.addEventListener("visibilitychange", onVisibilityChange);
  window.addEventListener("pagehide", onPageHide);
  window.addEventListener("load", onLoad);

  let observer: PerformanceObserver | undefined;
  try {
    observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        if (entry.entryType === "longtask" || entry.duration >= 500) {
          logEvent("warn", "performance.slow_entry", {
            entryType: entry.entryType,
            name: entry.name,
            startMs: Math.round(entry.startTime * 100) / 100,
            durationMs: Math.round(entry.duration * 100) / 100,
          });
        }
      }
    });
    observer.observe({ entryTypes: ["longtask", "resource"] });
  } catch {
    logEvent("debug", "performance.observer.unsupported");
  }

  const heartbeatTimer = window.setInterval(() => {
    const memory = (performance as Performance & { memory?: { usedJSHeapSize: number; totalJSHeapSize: number; jsHeapSizeLimit: number } }).memory;
    logEvent("info", "performance.heartbeat", {
      uptimeMs: Math.round(performance.now() - startedAt),
      visibility: document.visibilityState,
      memory: memory ? {
        usedHeapBytes: memory.usedJSHeapSize,
        totalHeapBytes: memory.totalJSHeapSize,
        heapLimitBytes: memory.jsHeapSizeLimit,
      } : undefined,
    });
  }, 60_000);

  let disposed = false;
  const cleanup = () => {
    if (disposed) return;
    disposed = true;
    loggingLifecycleActive = false;
    loggingLifecycleGeneration += 1;
    document.removeEventListener("click", onClick, true);
    document.removeEventListener("change", onChange, true);
    document.removeEventListener("visibilitychange", onVisibilityChange);
    window.removeEventListener("pagehide", onPageHide);
    window.removeEventListener("load", onLoad);
    observer?.disconnect();
    window.clearInterval(heartbeatTimer);
    if (flushTimer !== undefined) {
      window.clearTimeout(flushTimer);
      flushTimer = undefined;
    }
    if (activeClientLoggingCleanup === cleanup) activeClientLoggingCleanup = undefined;
  };
  activeClientLoggingCleanup = cleanup;

  logEvent("info", "frontend.start", {
    userAgent: navigator.userAgent,
    language: navigator.language,
    viewport: { width: window.innerWidth, height: window.innerHeight, dpr: window.devicePixelRatio },
  });

  return cleanup;
}
