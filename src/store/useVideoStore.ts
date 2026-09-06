import { create } from "zustand";
import { dbExecute, dbSelect } from "../lib/db";
import { logEvent } from "../lib/logger";

export interface VideoParams {
  prompt: string;
  duration_s: number;
  model: string;
  aspectRatio: string;
  resolution: string;
  referencePath: string;
  musicEnabled: boolean;
  musicPath: string;
}

const DEFAULTS: VideoParams = {
  prompt: "",
  duration_s: 5,
  model: "grok-imagine-video",
  aspectRatio: "16:9",
  resolution: "720p",
  referencePath: "",
  musicEnabled: false,
  musicPath: "",
};

async function persistLastMusic(path: string) {
  try {
    await dbExecute(
      "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
      ["last_music", path],
    );
  } catch (error) {
    logEvent("error", "video.music_path.persist_failed", { error: String(error) });
  }
}

async function loadLastMusic(): Promise<string> {
  try {
    const rows = await dbSelect<{ value: string }[]>("SELECT value FROM settings WHERE key = ?", ["last_music"]);
    return rows.length ? rows[0].value : "";
  } catch (error) {
    logEvent("error", "video.music_path.load_failed", { error: String(error) });
    return "";
  }
}

interface VideoState extends VideoParams {
  set: (p: Partial<VideoParams>) => void;
  load: (p: Partial<VideoParams>) => void;
  saveMusic: (path: string) => void;
  initMusic: () => Promise<void>;
}

export const useVideoStore = create<VideoState>((set) => ({
  ...DEFAULTS,
  set: (p) => set(p),
  load: (p) => set((s) => ({ ...s, ...p })),
  saveMusic: (path) => {
    persistLastMusic(path);
    set({ musicPath: path });
    logEvent("info", "video.music_path.changed", { path });
  },
  initMusic: async () => {
    const last = await loadLastMusic();
    if (last) set({ musicEnabled: true, musicPath: last });
  },
}));
