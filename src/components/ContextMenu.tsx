import { Fragment, useEffect, useLayoutEffect, useRef, useState } from "react";
import { ArrowRightLeft, Copy, ExternalLink, FolderOpen, Maximize2, Minimize2 } from "lucide-react";
import { useEscapeKey } from "../lib/useEscapeKey";

export interface MenuItem {
  label: string;
  icon?: React.ReactNode;
  onClick: () => void;
  /** Draws a separator above the item, starting a new group. */
  divider?: boolean;
}

export function ContextMenu({
  x,
  y,
  items,
  onClose,
}: {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState({ top: y, left: x });
  useEscapeKey(onClose);

  // Keep the whole menu inside the window, measured before it is painted.
  useLayoutEffect(() => {
    const node = ref.current;
    if (!node) return;
    setPosition({
      top: Math.max(8, Math.min(y, window.innerHeight - node.offsetHeight - 8)),
      left: Math.max(8, Math.min(x, window.innerWidth - node.offsetWidth - 8)),
    });
  }, [x, y, items.length]);

  useEffect(() => {
    const outside = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    window.addEventListener("mousedown", outside);
    return () => window.removeEventListener("mousedown", outside);
  }, [onClose]);

  return (
    <div
      ref={ref}
      role="menu"
      className="fixed z-50 max-h-[calc(100vh-16px)] min-w-40 overflow-y-auto rounded-lg border border-slate-700 bg-slate-900 py-1 shadow-2xl"
      style={position}
    >
      {items.map((it, i) => (
        <Fragment key={i}>
          {it.divider && i > 0 && <div role="separator" className="my-1 border-t border-slate-800" />}
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              it.onClick();
              onClose();
            }}
            className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs text-slate-200 transition hover:bg-slate-800 focus-visible:bg-slate-800 focus-visible:outline-none"
          >
            {it.icon}
            {it.label}
          </button>
        </Fragment>
      ))}
    </div>
  );
}

export const menuIcons = {
  preview: <Maximize2 size={13} className="text-slate-400" />,
  reference: <FolderOpen size={13} className="text-slate-400" />,
  copy: <Copy size={13} className="text-slate-400" />,
  open: <ExternalLink size={13} className="text-slate-400" />,
  compress: <Minimize2 size={13} className="text-slate-400" />,
  convert: <ArrowRightLeft size={13} className="text-slate-400" />,
};
