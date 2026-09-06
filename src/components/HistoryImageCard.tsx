import { useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { generationParamsSummary, inspectImageCached, latestDisplayPath, subscribeImageInfo } from "../lib/imageCompress";
import { logEvent } from "../lib/logger";
import type { ImageFileInfo } from "../lib/ipc";
import type { LibAsset } from "../store/useLibraryStore";

export default function HistoryImageCard({
  asset,
  onOpen,
  onMenu,
  tall = false,
}: {
  asset: LibAsset;
  onOpen: (displayPath?: string) => void;
  onMenu: (x: number, y: number) => void;
  tall?: boolean;
}) {
  const [info, setInfo] = useState<ImageFileInfo | null>(null);
  const [hover, setHover] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      void inspectImageCached(asset.asset.path)
        .then((value) => {
          if (!cancelled) setInfo(value);
        })
        .catch((error) => logEvent("warn", "image.inspect_failed", { error: String(error) }));
    };
    load();
    const unsubscribe = subscribeImageInfo((path) => {
      if (path === asset.asset.path) load();
    });
    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, [asset.asset.path]);

  const displayPath = info ? latestDisplayPath(info) : asset.asset.path;
  const shown = info?.variants.find((item) => item.path === displayPath) ?? info?.variants[0];
  const variants = info?.variants.filter((item) => item.level !== "original") ?? [];

  return (
    <div
      className={`relative flex items-center justify-center overflow-hidden rounded-lg border border-slate-700 bg-slate-800/60 ${tall ? "min-h-[320px]" : "aspect-square"}`}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
    >
      <img
        src={convertFileSrc(displayPath)}
        alt={asset.source}
        onClick={() => onOpen(displayPath)}
        onContextMenu={(event) => {
          event.preventDefault();
          onMenu(event.clientX, event.clientY);
        }}
        className="max-h-full max-w-full cursor-pointer object-contain transition hover:scale-105"
      />
      {hover && info && (
        <div className="pointer-events-none absolute inset-x-1 bottom-1 z-10 rounded-md border border-slate-600/80 bg-slate-950/92 p-2 text-[10px] leading-relaxed text-slate-200 shadow-xl">
          <div className="truncate text-slate-100">{shown?.fileName ?? info.fileName}</div>
          <div className="truncate text-slate-400">{info.directory}</div>
          <div>{shown?.kb ?? info.kb} KB{info.width && info.height ? ` · ${info.width}×${info.height}` : ""}</div>
          <div className="truncate text-slate-400">{generationParamsSummary({ ...(asset.params ?? {}), model: asset.model })}</div>
          <div className="mt-1 text-slate-500">同目录产物 {info.variants.length} 个，展示最新</div>
          {variants.slice(0, 3).map((item) => (
            <div key={item.path} className="truncate text-slate-500">{item.fileName} · {item.kb} KB</div>
          ))}
        </div>
      )}
    </div>
  );
}
