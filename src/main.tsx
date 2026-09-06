import React from "react";
import ReactDOM from "react-dom/client";
import type { Root } from "react-dom/client";
import App from "./App";
import "./index.css";
import { initClientLogging, logEvent } from "./lib/logger";

const disposeClientLogging = initClientLogging();

function showError(msg: string) {
  let el = document.getElementById("err-overlay");
  if (!el) {
    el = document.createElement("div");
    el.id = "err-overlay";
    el.style.cssText =
      "position:fixed;inset:0;z-index:99999;background:#0b1120;color:#f87171;padding:28px;font:14px/1.6 ui-monospace,monospace;white-space:pre-wrap;overflow:auto";
    document.body.appendChild(el);
  }
  el.textContent = "运行时错误:\n" + msg;
}

const onWindowError = (e: ErrorEvent) => {
  const message = String((e as ErrorEvent).error?.message ?? e.message);
  logEvent("error", "frontend.uncaught_error", { message, filename: e.filename, line: e.lineno, column: e.colno });
  showError(message);
};
const onUnhandledRejection = (e: PromiseRejectionEvent) => {
  const message = String(e.reason);
  logEvent("error", "frontend.unhandled_rejection", { message });
  showError(message);
};
window.addEventListener("error", onWindowError);
window.addEventListener("unhandledrejection", onUnhandledRejection);

let root: Root | undefined;
let disposed = false;
const disposeApp = () => {
  if (disposed) return;
  disposed = true;
  try {
    root?.unmount();
  } finally {
    disposeClientLogging();
    window.removeEventListener("error", onWindowError);
    window.removeEventListener("unhandledrejection", onUnhandledRejection);
  }
};

if (import.meta.hot) {
  import.meta.hot.dispose(disposeApp);
}

try {
  const renderStarted = performance.now();
  root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);
  root.render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
  logEvent("info", "frontend.react_render_scheduled", { durationMs: performance.now() - renderStarted });
} catch (e) {
  logEvent("error", "frontend.bootstrap_failed", { error: String(e) });
  showError(String(e));
}
