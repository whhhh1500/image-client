import type { AssetRef } from "../types";
import { loggedInvoke } from "./logger";

export type { AssetRef };

export interface AppInfo {
  name: string;
  version: string;
  platform: string;
}

export interface ConfigStatus {
  imageReady: boolean;
  videoReady: boolean;
  llmReady: boolean;
  imageApiUrl?: string;
  videoApiUrl?: string;
  imageModel: string;
  videoModel: string;
  llmModel: string;
  outputDir: string;
  source: "db" | "env" | "api" | "none";
}

export interface SaveConfigRequest {
  imageApiUrl: string;
  imageApiKey: string;
  imageApiModel: string;
  videoApiUrl: string;
  videoApiKey: string;
  videoApiModel: string;
  llmApiUrl: string;
  llmApiKey: string;
  llmApiModel: string;
  outputDir: string;
}

export interface AssetIn {
  path: string;
  kind: string;
}

export interface RunNodeRequest {
  nodeType: string;
  category: string;
  config: Record<string, unknown>;
  inputAssets: AssetIn[];
}

export interface RunResult {
  assets: AssetRef[];
}

export interface ProviderInfo {
  id: string;
  name: string;
  active: boolean;
  capabilities: string[];
}

export const appInfo = (): Promise<AppInfo> => loggedInvoke<AppInfo>("app_info");

export const configStatus = (): Promise<ConfigStatus> =>
  loggedInvoke<ConfigStatus>("config_status");

export const saveConfig = (config: SaveConfigRequest): Promise<ConfigStatus> =>
  loggedInvoke<ConfigStatus>("save_config", { config });

export const listImageModels = (): Promise<string[]> =>
  loggedInvoke<string[]>("list_image_models");

export const listVideoModels = (): Promise<string[]> =>
  loggedInvoke<string[]>("list_video_models");

export const listProviders = (): Promise<ProviderInfo[]> =>
  loggedInvoke<ProviderInfo[]>("list_providers");

export const setActiveProvider = (id: string): Promise<ProviderInfo[]> =>
  loggedInvoke<ProviderInfo[]>("set_active_provider", { id });

export const runNode = (req: RunNodeRequest): Promise<RunResult> =>
  loggedInvoke<RunResult>("run_node", { req });

export const runVideo = (req: RunNodeRequest): Promise<RunResult> =>
  loggedInvoke<RunResult>("run_video", { req });

export const importRefImage = (src: string): Promise<string> =>
  loggedInvoke<string>("import_ref_image", { src });

export const llmChat = (system: string, user: string, model?: string): Promise<string> =>
  loggedInvoke<string>("llm_chat", { system, user, model });

export interface AgentToolDef {
  name: string;
  description: string;
  system: string;
}

export const agentRun = (
  system: string,
  user: string,
  model: string | undefined,
  tools: AgentToolDef[],
): Promise<string> => loggedInvoke<string>("agent_run", { system, user, model, tools });

export const saveText = (label: string, text: string, model?: string): Promise<AssetRef> =>
  loggedInvoke<AssetRef>("save_text", { label, text, model });

export const readTextAsset = (path: string): Promise<string> =>
  loggedInvoke<string>("read_text_asset", { path });

export const saveMediaAsset = (kind: string, ext: string, data: Uint8Array): Promise<AssetRef> =>
  loggedInvoke<AssetRef>("save_media_asset", { kind, ext, data });

export const cachePromptlibImage = (fileName: string): Promise<string> =>
  loggedInvoke<string>("cache_promptlib_image", { fileName });

export const listCachedPromptlibImages = (): Promise<string[]> =>
  loggedInvoke<string[]>("list_cached_promptlib_images");

export type CompressLevel = "lossless" | "q80" | "q60";

export interface ImageVariantInfo {
  path: string;
  fileName: string;
  level: string;
  bytes: number;
  kb: number;
  modifiedAt: number;
}

export interface ImageFileInfo {
  path: string;
  directory: string;
  fileName: string;
  bytes: number;
  kb: number;
  width?: number;
  height?: number;
  format?: string;
  displayPath: string;
  variants: ImageVariantInfo[];
}

export const inspectImageFile = (path: string): Promise<ImageFileInfo> =>
  loggedInvoke<ImageFileInfo>("inspect_image_file", { path });

export const compressImageFile = (path: string, level: CompressLevel): Promise<AssetRef> =>
  loggedInvoke<AssetRef>("compress_image_file", { path, level });

export type ConvertFormat = "jpg" | "png" | "webp" | "bmp" | "gif";

export const convertImageFile = (path: string, format: ConvertFormat): Promise<AssetRef> =>
  loggedInvoke<AssetRef>("convert_image_file", { path, format });

export interface CleanupResult {
  removed: number;
  skipped: number;
  failed: number;
}

export const cleanupVideoSegments = (paths: string[]): Promise<CleanupResult> =>
  loggedInvoke<CleanupResult>("cleanup_video_segments", { paths });

export const logsDir = (): Promise<string> => loggedInvoke<string>("logs_dir");

export const markHistoryListenerReady = (): Promise<void> =>
  loggedInvoke<void>("mark_history_listener_ready");

export const acknowledgeHistoryRevision = (revision: number): Promise<void> =>
  loggedInvoke<void>("acknowledge_history_revision", { revision });
