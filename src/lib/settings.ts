import { dbExecute, dbSelect } from "./db";
import { logEvent } from "./logger";
import { saveConfig, type ConfigStatus, type SaveConfigRequest } from "./ipc";

export interface ProfileConn {
  url: string;
  key: string;
  model: string;
}

export interface ConfigProfile {
  id: string;
  name: string;
  image: ProfileConn;
  video: ProfileConn;
}

export interface AppSettings {
  configs: ConfigProfile[];
  activeId: string | null;
  outputDir: string;
  llmUrl: string;
  llmKey: string;
  llmModel: string;
}

const KEY = "settings";
const DEFAULT_OUTPUT_DIR = "$DEFAULT_ASSETS";

/** Load all interface configs from the SQLite `settings` table. */
export async function loadSettings(): Promise<AppSettings> {
  const rows = await dbSelect<{ value: string }[]>(
    "SELECT value FROM settings WHERE key = ?",
    [KEY],
  );
  if (!rows.length) return { configs: [], activeId: null, outputDir: "", llmUrl: "", llmKey: "", llmModel: "gemini-3.7-flash" };
  try {
    const p = JSON.parse(rows[0].value) as Partial<AppSettings>;
    return {
      configs: p.configs ?? [],
      activeId: p.activeId ?? null,
      outputDir: p.outputDir === DEFAULT_OUTPUT_DIR ? "" : (p.outputDir ?? ""),
      llmUrl: p.llmUrl ?? "",
      llmKey: p.llmKey ?? "",
      llmModel: p.llmModel ?? "gemini-3.7-flash",
    };
  } catch (error) {
    logEvent("warn", "settings.parse_failed", { error: String(error) });
    return { configs: [], activeId: null, outputDir: "", llmUrl: "", llmKey: "", llmModel: "gemini-3.7-flash" };
  }
}

export function emptyProfile(i: number): ConfigProfile {
  return {
    id: `cfg_${Date.now()}_${i}`,
    name: `配置 ${i}`,
    image: { url: "", key: "", model: "gpt-image-2" },
    video: { url: "", key: "", model: "kling-video-v3" },
  };
}

/** Persist configs + active + output dir + llm, then apply the active config to Rust. */
export async function saveSettings(s: AppSettings): Promise<ConfigStatus | null> {
  // 密钥输入框默认留空表示“保持不变”。保存其他设置时不能把已持久化的
  // LLM Key 覆盖为空，否则应用重启后会丢失配置。
  const current = await loadSettings();
  const persisted: AppSettings = {
    ...s,
    llmKey: s.llmKey.trim() || current.llmKey,
  };
  await dbExecute(
    "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    [KEY, JSON.stringify(persisted)],
  );
  logEvent("info", "settings.saved", { profileCount: persisted.configs.length, activeId: persisted.activeId, llmModel: persisted.llmModel, outputDir: persisted.outputDir });
  return applyActive(persisted);
}

/** Push the active (or first) config + output dir + llm to Rust. Empty values
 * keep whatever the backend already holds. */
export async function applyActive(s: AppSettings): Promise<ConfigStatus | null> {
  const active = s.configs.find((c) => c.id === s.activeId) ?? s.configs[0];
  const req: SaveConfigRequest = {
    imageApiUrl: active?.image.url ?? "",
    imageApiKey: active?.image.key ?? "",
    imageApiModel: active?.image.model ?? "gpt-image-2",
    videoApiUrl: active?.video.url ?? "",
    videoApiKey: active?.video.key ?? "",
    videoApiModel: active?.video.model ?? "kling-video-v3",
    llmApiUrl: s.llmUrl ?? "",
    llmApiKey: s.llmKey ?? "",
    llmApiModel: s.llmModel ?? "gemini-3.7-flash",
    outputDir: s.outputDir.trim() || DEFAULT_OUTPUT_DIR,
  };
  return saveConfig(req);
}
