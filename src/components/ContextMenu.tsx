import { useEffect, useRef } from "react";
import { Copy, ExternalLink, FolderOpen, Maximize2, Minimize2 } from "lucide-react";

export interface MenuItem {
  label: string;
  icon?: React.ReactNode;
  onClick: () => void;
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

  useEffect(() => {
    const outside = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("mousedown", outside);
    window.addEventListener("keydown", esc);
    return () => {
      window.removeEventListener("mousedown", outside);
      window.removeEventListener("keydown", esc);
    };
  }, [onClose]);

  return (
    <div
      ref={ref}
      className="fixed z-50 min-w-40 overflow-hidden rounded-lg border border-slate-700 bg-slate-900 py-1 shadow-2xl"
      style={{
        top: Math.max(8, Math.min(y, window.innerHeight - 320)),
        left: Math.max(8, Math.min(x, window.innerWidth - 200)),
      }}
    >
      {items.map((it, i) => (
        <button
          key={i}
          onClick={() => {
            it.onClick();
            onClose();
          }}
          className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs text-slate-200 transition hover:bg-slate-800"
        >
          {it.icon}
          {it.label}
        </button>
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
};
