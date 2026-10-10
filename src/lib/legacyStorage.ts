import { invoke, isDesktop } from "./desktop";
import { logEvent } from "./logger";

/**
 * Restores UI state the Tauri build kept in localStorage (drafts, last-opened
 * workflows, presets). The page origin changed with the desktop shell, so the
 * backend reads the old entries once and this writes the ones the new origin
 * does not have yet. Runs before the first render, because components read
 * localStorage while mounting.
 */
export async function restoreLegacyLocalStorage(): Promise<void> {
  if (!isDesktop()) return;
  try {
    const entries = await invoke<Record<string, string>>("legacy_local_storage_take");
    let restored = 0;
    for (const [key, value] of Object.entries(entries)) {
      if (localStorage.getItem(key) !== null) continue;
      localStorage.setItem(key, value);
      restored += 1;
    }
    if (restored > 0) logEvent("info", "legacy_local_storage.restored", { restored, total: Object.keys(entries).length });
  } catch (error) {
    // Losing old UI state is not worth blocking startup for.
    logEvent("warn", "legacy_local_storage.restore_failed", { error: String(error) });
  }
}
