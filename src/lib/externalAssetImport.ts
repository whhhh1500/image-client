import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { encodeBase64 } from "./base64";
import { assetImportFiles, type AssetImportFileInput, type ExternalAssetImportEntry } from "./ipc";

export type ExternalImportSource = { path: string } | { file: File };

export interface ImportExternalAssetsInput {
  projectId: string;
  importEntry: ExternalAssetImportEntry;
  files: ExternalImportSource[];
  params?: Record<string, unknown>;
  uploadBatchId?: string;
}

const MAX_IMPORT_FILES = 8;
const MAX_BROWSER_BATCH_BYTES = 512 * 1024 * 1024;

function createUploadBatchId() {
  return typeof globalThis.crypto?.randomUUID === "function"
    ? globalThis.crypto.randomUUID()
    : `upload-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

async function sourceToInput(source: ExternalImportSource): Promise<AssetImportFileInput> {
  if ("path" in source) return { source: "path", path: source.path };
  return {
    source: "bytes",
    fileName: source.file.name,
    // Base64 rather than a nested Uint8Array: Tauri would otherwise expand the
    // buffer into a JSON number array (one string per byte).
    dataBase64: encodeBase64(new Uint8Array(await source.file.arrayBuffer())),
  };
}

/**
 * Imports selected external files as project-scoped library assets. The native
 * command owns file write plus SQLite registration, so callers must not call
 * persistAssets for these returned assets.
 */
export async function importExternalAssets(input: ImportExternalAssetsInput): Promise<LibAsset[]> {
  if (!input.projectId.trim()) throw new Error("导入资产必须指定项目");
  if (!input.files.length) return [];
  if (input.files.length > MAX_IMPORT_FILES) throw new Error("一次请选择 1 到 8 个文件");
  // Browser File.arrayBuffer() would otherwise allocate the entire multi-select
  // before the native command gets a chance to reject it. Paths stay native so
  // their sizes are preflighted there without reading their contents.
  const browserBytes = input.files.reduce((total, source) => {
    if (!("file" in source)) return total;
    return total + source.file.size;
  }, 0);
  if (browserBytes > MAX_BROWSER_BATCH_BYTES) throw new Error("导入批次超过 512 MiB 大小限制");
  const uploadBatchId = input.uploadBatchId ?? createUploadBatchId();
  const files = await Promise.all(input.files.map(sourceToInput));
  const imported = await assetImportFiles({
    projectId: input.projectId,
    importEntry: input.importEntry,
    uploadBatchId,
    files,
    params: input.params ?? {},
  });
  const libraryAssets: LibAsset[] = imported.map((item) => ({
    asset: item.asset,
    source: item.source,
    projectId: item.projectId,
    params: item.params,
    createdAt: item.createdAt,
  }));
  useLibraryStore.getState().addLibraryAssets(libraryAssets);
  return libraryAssets;
}
