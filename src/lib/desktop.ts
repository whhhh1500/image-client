/** Desktop shell adapter: the only place that talks to the Wails runtime. */

declare global {
  interface Window {
    /** Bound Go objects injected by Wails. */
    go?: {
      app?: {
        Bridge?: {
          /** Dispatches a named command to the Go side; rejects with the error message. */
          Invoke: (command: string, args: object) => Promise<any>;
        };
      };
    };
    /** Wails event runtime. `EventsOn` returns the unsubscribe function. */
    runtime?: {
      EventsOn: (event: string, callback: (payload: any) => void) => () => void;
    };
  }
}

export type UnlistenFn = () => void;

const UNAVAILABLE = "桌面运行时不可用";

/** True when running inside the Wails desktop shell. */
export function isDesktop(): boolean {
  return Boolean(window.go?.app?.Bridge);
}

/**
 * Calls a Go command through the Bridge. A failure rejects with the backend's
 * message string, as Tauri's invoke did: the runtime wraps it in an Error,
 * which would put an "Error: " prefix on every message the UI shows.
 */
export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const bridge = window.go?.app?.Bridge;
  if (!bridge) return Promise.reject(new Error(UNAVAILABLE));
  return bridge.Invoke(command, args ?? {}).catch((error: unknown) => {
    throw error instanceof Error ? error.message : error;
  });
}

/** Subscribes to a backend event; resolves with the unsubscribe function. */
export function listen<T>(event: string, handler: (event: { payload: T }) => void): Promise<UnlistenFn> {
  const runtime = window.runtime;
  if (!runtime) return Promise.reject(new Error(UNAVAILABLE));
  const off = runtime.EventsOn(event, (payload: T) => handler({ payload }));
  return Promise.resolve(off);
}

/** Turns a local file path into a URL the webview can load. */
export function fileSrc(path: string): string {
  return "/__file/" + encodeURIComponent(path);
}

export interface DialogFilter {
  name: string;
  extensions: string[];
}

/** Native open dialog; returns the chosen path(s) or null when cancelled. */
export function openDialog(options: { multiple?: boolean; directory?: boolean; filters?: DialogFilter[] }): Promise<string | string[] | null> {
  return invoke("dialog_open", { options });
}

/** Native confirm dialog; resolves true when the user accepts. */
export function confirmDialog(message: string, options?: { title?: string; okLabel?: string; cancelLabel?: string }): Promise<boolean> {
  return invoke("dialog_confirm", { message, ...options });
}

/** Reveals a file in the system file manager. */
export function revealItemInDir(path: string): Promise<void> {
  return invoke("reveal_item_in_dir", { path });
}

/** Opens a file with its default application. */
export function openPath(path: string): Promise<void> {
  return invoke("open_path", { path });
}

/** Opens a URL in the system browser. */
export function openUrl(url: string): Promise<void> {
  return invoke("open_url", { url });
}
