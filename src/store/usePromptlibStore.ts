import { create } from "zustand";
import { dbExecute, dbSelect } from "../lib/db";
import { logEvent } from "../lib/logger";
import type { PromptlibOverride } from "../lib/promptlib";

const KEY = "promptlib_overrides";

interface PromptlibState {
  overrides: Record<string, PromptlibOverride>;
  loaded: boolean;
  load: () => Promise<void>;
  saveOverride: (id: string, prompt: string, jsonPrompt?: string) => Promise<void>;
  restore: (id: string) => Promise<void>;
}

async function persist(overrides: Record<string, PromptlibOverride>) {
  await dbExecute(
    "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    [KEY, JSON.stringify(overrides)],
  );
}

function parseOverrides(raw: string): Record<string, PromptlibOverride> {
  try {
    const value = JSON.parse(raw) as Record<string, unknown>;
    const out: Record<string, PromptlibOverride> = {};
    for (const [id, item] of Object.entries(value ?? {})) {
      if (!item || typeof item !== "object") continue;
      const row = item as Record<string, unknown>;
      if (typeof row.prompt !== "string" || !row.prompt.trim()) continue;
      out[id] = {
        prompt: row.prompt,
        jsonPrompt: typeof row.jsonPrompt === "string" ? row.jsonPrompt : undefined,
        updatedAt: typeof row.updatedAt === "number" ? row.updatedAt : Date.now(),
      };
    }
    return out;
  } catch {
    return {};
  }
}

export const usePromptlibStore = create<PromptlibState>((set, get) => ({
  overrides: {},
  loaded: false,
  load: async () => {
    try {
      const rows = await dbSelect<{ value: string }[]>("SELECT value FROM settings WHERE key = ?", [KEY]);
      set({ overrides: rows.length ? parseOverrides(rows[0].value) : {}, loaded: true });
    } catch (error) {
      logEvent("error", "promptlib.load_failed", { error: String(error) });
      set({ loaded: true });
    }
  },
  saveOverride: async (id, prompt, jsonPrompt) => {
    const previous = get().overrides;
    const next = {
      ...previous,
      [id]: { prompt, jsonPrompt, updatedAt: Date.now() },
    };
    set({ overrides: next });
    try {
      await persist(next);
      logEvent("info", "promptlib.override_saved", { id });
    } catch (error) {
      set({ overrides: previous });
      logEvent("error", "promptlib.override_save_failed", { id, error: String(error) });
      throw error;
    }
  },
  restore: async (id) => {
    const previous = get().overrides;
    const next = { ...previous };
    delete next[id];
    set({ overrides: next });
    try {
      await persist(next);
      logEvent("info", "promptlib.override_restored", { id });
    } catch (error) {
      set({ overrides: previous });
      logEvent("error", "promptlib.override_restore_failed", { id, error: String(error) });
      throw error;
    }
  },
}));
