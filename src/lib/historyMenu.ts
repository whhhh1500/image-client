import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { menuIcons, type MenuItem } from "../components/ContextMenu";
import { compressHistoryImage, convertHistoryImage, refreshImageInfo } from "./imageCompress";
import type { ConvertFormat } from "./ipc";
import { logEvent } from "./logger";
import type { LibAsset } from "../store/useLibraryStore";

/** Writes that are already running, keyed by asset path + operation. */
const inFlightWrites = new Set<string>();

export function imageHistoryMenu(input: {
  asset: LibAsset;
  onPreview: (asset: LibAsset) => void;
  onReference?: (asset: LibAsset) => void;
  onCompressed?: (asset: LibAsset) => void;
  onRefresh?: () => void;
}): MenuItem[] {
  const afterWrite = async (asset: LibAsset) => {
    await refreshImageInfo(input.asset.asset.path);
    input.onCompressed?.(asset);
    input.onRefresh?.();
  };
  const runWrite = (key: string, eventName: string, failureLabel: string, task: () => Promise<LibAsset>) => {
    // Two writes in the same second produce the same output file name but two
    // asset rows; refuse the duplicate instead of silently overwriting.
    if (inFlightWrites.has(key)) return;
    inFlightWrites.add(key);
    void task()
      .then(afterWrite)
      .catch((error) => {
        logEvent("warn", eventName, { error: String(error) });
        window.alert(`${failureLabel}失败：${error}`);
      })
      .finally(() => inFlightWrites.delete(key));
  };
  const compress = (label: string, level: "lossless" | "q80" | "q60") => ({
    label,
    icon: menuIcons.compress,
    onClick: () => runWrite(
      `compress:${input.asset.asset.path}:${level}`,
      "image.compress_failed",
      "压缩",
      () => compressHistoryImage(input.asset, level),
    ),
  });
  const convert = (format: ConvertFormat) => ({
    label: `转换为 ${format.toUpperCase()}`,
    icon: menuIcons.compress,
    onClick: () => runWrite(
      `convert:${input.asset.asset.path}:${format}`,
      "image.convert_failed",
      "转换",
      () => convertHistoryImage(input.asset, format),
    ),
  });
  return [
    { label: "放大预览", icon: menuIcons.preview, onClick: () => input.onPreview(input.asset) },
    ...(input.onReference ? [{ label: "添加为参考图", icon: menuIcons.reference, onClick: () => input.onReference?.(input.asset) }] : []),
    compress("无损压缩", "lossless"),
    compress("有损压缩 80%", "q80"),
    compress("有损压缩 60%", "q60"),
    convert("jpg"),
    convert("png"),
    convert("webp"),
    convert("bmp"),
    convert("gif"),
    { label: "复制到粘贴板", icon: menuIcons.copy, onClick: () => void navigator.clipboard?.writeText(input.asset.asset.path).catch((error) => logEvent("warn", "clipboard.write_failed", { error: String(error) })) },
    { label: "打开所在文件夹", icon: menuIcons.open, onClick: () => void revealItemInDir(input.asset.asset.path).catch((error) => logEvent("warn", "asset.reveal_failed", { error: String(error) })) },
  ];
}
