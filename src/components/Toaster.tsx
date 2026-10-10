import { Loader2, X } from "lucide-react";
import { useToastStore, type ToastTone } from "../store/useToastStore";

const toneClass: Record<ToastTone, string> = {
  progress: "border-slate-600 bg-slate-900/95 text-slate-200",
  success: "border-emerald-400/30 bg-slate-900/95 text-emerald-100",
  info: "border-amber-300/30 bg-slate-900/95 text-amber-100",
  error: "border-rose-400/40 bg-slate-900/95 text-rose-100",
};

/** Toasts above the status bar, newest last. */
export default function Toaster() {
  const toasts = useToastStore((state) => state.toasts);
  const dismiss = useToastStore((state) => state.dismiss);
  return (
    <div aria-live="polite" className="pointer-events-none fixed bottom-10 right-4 z-[80] flex w-80 max-w-[calc(100vw-2rem)] flex-col gap-2">
      {toasts.map((item) => (
        <div key={item.id} role={item.tone === "error" ? "alert" : "status"} className={`pointer-events-auto flex items-start gap-2 rounded-lg border px-3 py-2 text-xs shadow-xl backdrop-blur ${toneClass[item.tone]}`}>
          {item.tone === "progress" && <Loader2 size={13} className="mt-0.5 shrink-0 animate-spin" />}
          <span className="min-w-0 flex-1 break-words leading-5">{item.text}</span>
          {item.tone !== "progress" && (
            <button type="button" aria-label="关闭提示" onClick={() => dismiss(item.id)} className="shrink-0 rounded p-0.5 text-slate-400 hover:text-white">
              <X size={12} />
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
