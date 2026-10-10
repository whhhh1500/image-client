import { revealItemInDir } from "./desktop";
import { menuIcons, type MenuItem } from "../components/ContextMenu";
import { compressHistoryImage, convertHistoryImage, formatBytes, type ImageWriteResult } from "./imageCompress";
import type { CompressLevel, ConvertFormat } from "./ipc";
import { logEvent } from "./logger";
import { errorText } from "./errors";
import { toast } from "../store/useToastStore";
import type { LibAsset } from "../store/useLibraryStore";

/** Writes that are already running, keyed by asset path + operation. */
const inFlightWrites = new Set<string>();

/**
 * Label of the single-asset reference action. It appends to the current
 * references (never replaces them), so every entry point should use this text.
 */
export const ADD_REFERENCE_LABEL = "添加为参考图";

const COMPRESS_LEVELS: Array<{ level: CompressLevel; label: string }> = [
  { level: "lossless", label: "无损压缩" },
  { level: "q80", label: "有损压缩 80%" },
  { level: "q60", label: "有损压缩 60%" },
];
const CONVERT_FORMATS: ConvertFormat[] = ["jpg", "png", "webp", "bmp", "gif"];

/** The image format by file extension, jpeg spelled jpg. */
function imageFormat(path: string): string {
  const ext = /\.([A-Za-z0-9]+)$/.exec(path)?.[1].toLowerCase() ?? "";
  return ext === "jpeg" ? "jpg" : ext;
}

/** The backend declines, rather than fails, when the image cannot get smaller. */
function isDeclined(text: string): boolean {
  return text.startsWith("压缩后不会更小") || text.startsWith("JPEG 无法无损压缩");
}

/** `label` followed by a Chinese word, spaced after a Latin ending ("GIF 失败"). */
function before(label: string): string {
  return /[A-Za-z0-9%]$/.test(label) ? `${label} ` : label;
}

function sizeChange({ fromBytes, toBytes }: ImageWriteResult): string {
  if (fromBytes === undefined || toBytes === undefined) return "";
  const saved = fromBytes > 0 ? Math.round((1 - toBytes / fromBytes) * 100) : 0;
  return `：${formatBytes(fromBytes)} → ${formatBytes(toBytes)}${saved > 0 ? `（减少 ${saved}%）` : ""}`;
}

export function imageHistoryMenu(input: {
  asset: LibAsset;
  onPreview: (asset: LibAsset) => void;
  onReference?: (asset: LibAsset) => void;
}): MenuItem[] {
  const path = input.asset.asset.path;
  const format = imageFormat(path);
  const runWrite = (key: string, label: string, eventName: string, task: () => Promise<ImageWriteResult>, done: string) => {
    // Two writes in the same millisecond would share an output name; one at a
    // time per image and operation also keeps repeated clicks from piling up.
    if (inFlightWrites.has(key)) {
      toast("info", `正在${label}，请等待完成`);
      return;
    }
    inFlightWrites.add(key);
    const id = toast("progress", `正在${label}…`);
    void task()
      .then((result) => toast("success", `${done}${sizeChange(result)}，已保存在原图所在文件夹`, id))
      .catch((error) => {
        const text = errorText(error);
        logEvent("warn", eventName, { error: text });
        if (isDeclined(text)) toast("info", text, id);
        else toast("error", `${before(label)}失败：${text}`, id);
      })
      .finally(() => inFlightWrites.delete(key));
  };
  // JPEG has no lossless re-encoding, and converting to the format the image
  // already has only re-encodes it, so neither is offered.
  const compressItems = COMPRESS_LEVELS
    .filter(({ level }) => !(level === "lossless" && format === "jpg"))
    .map(({ level, label }, index): MenuItem => ({
      label,
      icon: menuIcons.compress,
      divider: index === 0,
      onClick: () => runWrite(`compress:${path}:${level}`, label, "image.compress_failed", () => compressHistoryImage(input.asset, level), `${before(label)}完成`),
    }));
  const convertItems = CONVERT_FORMATS
    .filter((target) => target !== format)
    .map((target, index): MenuItem => {
      const label = `转换为 ${target.toUpperCase()}`;
      return {
        label,
        icon: menuIcons.convert,
        divider: index === 0,
        onClick: () => runWrite(`convert:${path}:${target}`, label, "image.convert_failed", () => convertHistoryImage(input.asset, target), `已${label}`),
      };
    });
  return [
    { label: "放大预览", icon: menuIcons.preview, onClick: () => input.onPreview(input.asset) },
    ...(input.onReference ? [{ label: ADD_REFERENCE_LABEL, icon: menuIcons.reference, onClick: () => input.onReference?.(input.asset) }] : []),
    ...compressItems,
    ...convertItems,
    { label: "复制文件路径", icon: menuIcons.copy, divider: true, onClick: () => void navigator.clipboard?.writeText(path).catch((error) => logEvent("warn", "clipboard.write_failed", { error: String(error) })) },
    { label: "打开所在文件夹", icon: menuIcons.open, onClick: () => void revealItemInDir(path).catch((error) => logEvent("warn", "asset.reveal_failed", { error: String(error) })) },
  ];
}
