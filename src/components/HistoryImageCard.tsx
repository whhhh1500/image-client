import { useEffect, useRef, useState } from "react";
import { fileSrc } from "../lib/desktop";
import { generationParamsSummary, inspectImageCached, latestDisplayPath, subscribeImageInfo } from "../lib/imageCompress";
import { logEvent } from "../lib/logger";
import type { ImageFileInfo } from "../lib/ipc";
import type { LibAsset } from "../store/useLibraryStore";

export default function HistoryImageCard({
  asset,
  onOpen,
  onMenu,
  tall = false,
  showLatestVariant = false,
}: {
  asset: LibAsset;
  onOpen: (displayPath?: string) => void;
  onMenu: (x: number, y: number) => void;
  tall?: boolean;
  /**
   * Show the newest compressed/converted variant in place of this image. Only
   * for lists of originals (the generate page's history); the asset library
   * lists every variant as its own card, which must show its own file.
   */
  showLatestVariant?: boolean;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const [info, setInfo] = useState<ImageFileInfo | null>(null);
  const [hover, setHover] = useState(false);
  // A history grid holds hundreds of cards and each inspection is an IPC round
  // trip that reads the file: only inspect once the card nears the viewport.
  // Without IntersectionObserver (e.g. jsdom) every card counts as visible.
  const [visible, setVisible] = useState(() => typeof IntersectionObserver === "undefined");

  useEffect(() => {
    if (visible) return;
    const node = rootRef.current;
    if (!node) return;
    const observer = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting)) return;
      observer.disconnect();
      setVisible(true);
    }, { rootMargin: "200px" });
    observer.observe(node);
    return () => observer.disconnect();
  }, [visible]);

  useEffect(() => {
    if (!visible) return;
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
  }, [asset.asset.path, visible]);

  const displayPath = info && showLatestVariant ? latestDisplayPath(info) : asset.asset.path;
  const shown = info?.variants.find((item) => item.path === (showLatestVariant ? displayPath : info.path)) ?? info?.variants[0];
  const variants = info?.variants.filter((item) => item.level !== "original") ?? [];

  return (
    <div
      ref={rootRef}
      className={`relative flex items-center justify-center overflow-hidden rounded-lg border border-slate-700 bg-slate-800/60 ${tall ? "min-h-[320px]" : "aspect-square"}`}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
    >
      {/* A button so the card opens with Enter/Space and the menu key; the file info shows on focus as on hover. */}
      <button
        type="button"
        onClick={() => onOpen(displayPath)}
        onContextMenu={(event) => {
          event.preventDefault();
          onMenu(event.clientX, event.clientY);
        }}
        onFocus={() => setHover(true)}
        onBlur={() => setHover(false)}
        className="flex h-full w-full items-center justify-center focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-cyan-300/70"
      >
        <img
          src={fileSrc(displayPath)}
          alt={asset.source}
          loading="lazy"
          decoding="async"
          className="max-h-full max-w-full cursor-pointer object-contain transition hover:scale-105"
        />
      </button>
      {hover && info && (
        <div className="pointer-events-none absolute inset-x-1 bottom-1 z-10 rounded-md border border-slate-600/80 bg-slate-950/92 p-2 text-[10px] leading-relaxed text-slate-200 shadow-xl">
          <div className="truncate text-slate-100">{shown?.fileName ?? info.fileName}</div>
          <div className="truncate text-slate-400">{info.directory}</div>
          <div>{shown?.kb ?? info.kb} KB{info.width && info.height ? ` · ${info.width}×${info.height}` : ""}</div>
          <div className="truncate text-slate-400">{generationParamsSummary({ ...(asset.params ?? {}), model: asset.model })}</div>
          <div className="mt-1 text-slate-500">同目录产物 {info.variants.length} 个{showLatestVariant ? "，展示最新" : ""}</div>
          {variants.slice(0, 3).map((item) => (
            <div key={item.path} className="truncate text-slate-500">{item.fileName} · {item.kb} KB</div>
          ))}
        </div>
      )}
    </div>
  );
}
