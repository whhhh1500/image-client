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

// Writes are serialized and always persist the state that is current at write
// time, so a slow write for one id cannot resurrect a map that still contains a
// rolled-back value for another id.
let writeChain: Promise<unknown> = Promise.resolve();
const serialize = <T,>(task: () => Promise<T>): Promise<T> => {
  const run = writeChain.then(task, task);
  writeChain = run.then(() => undefined, () => undefined);
  return run;
};

function parseOverrides(raw: string): Record<string, PromptlibOverride> {  try {
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
    const previous = get().overrides[id];
    const next: PromptlibOverride = { prompt, jsonPrompt, updatedAt: Date.now() };
    set((state) => ({ overrides: { ...state.overrides, [id]: next } }));
    try {
      await serialize(() => persist(get().overrides));
      logEvent("info", "promptlib.override_saved", { id });
    } catch (error) {
      // Roll back only this id, and only while our optimistic value is current:
      // a concurrent successful save for the same id must win.
      set((state) => {
        if (state.overrides[id]?.updatedAt !== next.updatedAt) return state;
        const reverted = { ...state.overrides };
        if (previous) reverted[id] = previous;
        else delete reverted[id];
        return { overrides: reverted };
      });
      logEvent("error", "promptlib.override_save_failed", { id, error: String(error) });
      throw error;
    }
  },
  restore: async (id) => {
    const previous = get().overrides[id];
    if (!previous) return;
    set((state) => {
      const next = { ...state.overrides };
      delete next[id];
      return { overrides: next };
    });
    try {
      await serialize(() => persist(get().overrides));
      logEvent("info", "promptlib.override_restored", { id });
    } catch (error) {
      set((state) => (state.overrides[id] ? state : { overrides: { ...state.overrides, [id]: previous } }));
      logEvent("error", "promptlib.override_restore_failed", { id, error: String(error) });
      throw error;
    }
  },
}));
