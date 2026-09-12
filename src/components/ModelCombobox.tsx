import { useEffect, useId, useMemo, useRef, useState } from "react";
import { ChevronDown, Loader2, RefreshCw } from "lucide-react";

const inputCls =
  "w-full rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-indigo-400 focus:ring-1 focus:ring-indigo-400/40";
const compactInputCls =
  "w-full rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-1.5 text-xs text-slate-100 outline-none transition focus:border-indigo-400 focus:ring-1 focus:ring-indigo-400/40";
const fetchBtnCls =
  "flex shrink-0 items-center gap-1 rounded-lg border border-slate-600 px-3 py-2 text-xs text-slate-200 transition hover:bg-slate-800 disabled:opacity-50";
const compactFetchBtnCls =
  "flex shrink-0 items-center gap-1 rounded-lg border border-slate-600 px-2.5 py-1.5 text-[11px] text-slate-200 transition hover:bg-slate-800 disabled:opacity-50";

/** Approximate height of the suggestion list, used to decide its direction. */
const LIST_HEIGHT = 280;

export interface ModelComboboxStatus {
  text: string;
  error: boolean;
}

/**
 * Model field shared by every place that lets the user pick a model: a
 * free-form text input with a suggestion list and, on the right, a button that
 * reads the gateway catalog.
 *
 * The dropdown only *suggests* names — whatever the user types is kept as-is,
 * so a gateway that exposes a model the app has never heard of still works.
 */
export default function ModelCombobox({
  label,
  value,
  options,
  onChange,
  onFetch,
  fetching = false,
  fetchLabel = "获取模型",
  placeholder,
  status,
  hint,
  labels,
  compact = false,
}: {
  /** Field name, also used for the accessible labels ("图像模型"). */
  label: string;
  value: string;
  options: string[];
  onChange: (value: string) => void;
  onFetch?: () => void;
  fetching?: boolean;
  fetchLabel?: string;
  placeholder?: string;
  status?: ModelComboboxStatus | null;
  hint?: string;
  /** Friendly names for catalog ids, e.g. `{ "kling-video-v3": "可灵 V3" }`. */
  labels?: Record<string, string>;
  /** Denser control for toolbars; the settings forms keep the default size. */
  compact?: boolean;
}) {
  const listId = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  // `null` means "not typing": the full catalog is offered instead of a
  // filter derived from the current value.
  const [query, setQuery] = useState<string | null>(null);
  const [active, setActive] = useState(-1);
  // Fields near the bottom of a scrolling dialog would otherwise have their
  // suggestion list clipped by the container.
  const [placement, setPlacement] = useState<"down" | "up">("down");

  const candidates = useMemo(() => {
    const unique: string[] = [];
    const seen = new Set<string>();
    for (const option of options) {
      const trimmed = option.trim();
      if (trimmed && !seen.has(trimmed)) {
        seen.add(trimmed);
        unique.push(trimmed);
      }
    }
    const needle = (query ?? "").trim().toLowerCase();
    return needle
      ? unique.filter(
          (option) =>
            option.toLowerCase().includes(needle) ||
            (labels?.[option] ?? "").toLowerCase().includes(needle),
        )
      : unique;
  }, [options, query, labels]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) {
        setOpen(false);
        setQuery(null);
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [open]);

  const openList = () => {
    const rect = rootRef.current?.getBoundingClientRect();
    setPlacement(rect && rect.bottom + LIST_HEIGHT > window.innerHeight ? "up" : "down");
    setOpen(true);
    setQuery(null);
    setActive(-1);
  };

  const commit = (next: string) => {
    onChange(next);
    setOpen(false);
    setQuery(null);
    setActive(-1);
  };

  const toggle = () => {
    if (open) {
      setOpen(false);
      setQuery(null);
    } else {
      openList();
    }
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!open) {
        openList();
        setActive(event.key === "ArrowDown" ? 0 : Math.max(candidates.length - 1, 0));
        return;
      }
      if (!candidates.length) return;
      const step = event.key === "ArrowDown" ? 1 : -1;
      setActive((current) => (current + step + candidates.length) % candidates.length);
      return;
    }
    if (event.key === "Enter") {
      if (open && active >= 0 && candidates[active]) {
        event.preventDefault();
        commit(candidates[active]);
      } else if (open) {
        setOpen(false);
        setQuery(null);
      }
      return;
    }
    if (event.key === "Escape" && open) {
      setOpen(false);
      setQuery(null);
      setActive(-1);
      return;
    }
    if (event.key === "Tab") {
      setOpen(false);
      setQuery(null);
    }
  };

  return (
    <div ref={rootRef} className="relative">
      <div className="flex gap-2">
        <div className="relative min-w-0 flex-1">
          <input
            className={`${compact ? compactInputCls : inputCls} pr-8`}
            role="combobox"
            aria-label={label}
            aria-expanded={open}
            aria-controls={listId}
            aria-autocomplete="list"
            value={value}
            placeholder={placeholder}
            spellCheck={false}
            autoComplete="off"
            onChange={(event) => {
              onChange(event.target.value);
              setQuery(event.target.value);
              setOpen(true);
              setActive(-1);
            }}
            onFocus={openList}
            onClick={openList}
            onKeyDown={onKeyDown}
          />
          <button
            type="button"
            aria-label={`展开${label}列表`}
            onClick={toggle}
            className="absolute right-1 top-1/2 -translate-y-1/2 rounded p-1 text-slate-400 transition hover:text-slate-100"
          >
            <ChevronDown size={14} className={open ? "rotate-180 transition" : "transition"} />
          </button>
        </div>
        {onFetch && (
          <button
            type="button"
            aria-label={`${fetchLabel}（${label}）`}
            onClick={onFetch}
            disabled={fetching}
            className={compact ? compactFetchBtnCls : fetchBtnCls}
          >
            {fetching ? <Loader2 size={13} className="animate-spin" /> : <RefreshCw size={13} />}
            {fetching ? "获取中…" : fetchLabel}
          </button>
        )}
      </div>

      {open && (
        <div
          id={listId}
          role="listbox"
          aria-label={`${label}候选`}
          className={`absolute left-0 right-0 z-20 max-h-56 overflow-y-auto rounded-lg border border-slate-700 bg-slate-900 shadow-2xl ${
            placement === "up" ? "bottom-full mb-1" : "mt-1"
          }`}
        >
          {candidates.length ? (
            candidates.map((option, index) => (
              <button
                key={option}
                type="button"
                role="option"
                aria-selected={option === value}
                onMouseEnter={() => setActive(index)}
                onClick={() => commit(option)}
                className={`block w-full px-3 py-1.5 text-left text-xs transition ${
                  option === value
                    ? "bg-indigo-500/20 text-indigo-100"
                    : index === active
                      ? "bg-slate-800 text-slate-100"
                      : "text-slate-300 hover:bg-slate-800"
                }`}
              >
                {labels?.[option] && labels[option] !== option ? (
                  <>
                    <span>{labels[option]}</span>
                    <span className="ml-1.5 text-[10px] text-slate-500">{option}</span>
                  </>
                ) : (
                  option
                )}
              </button>
            ))
          ) : (
            <div className="px-3 py-2 text-[11px] text-slate-500">
              {options.length ? "没有匹配的模型，可直接使用输入内容" : "暂无候选模型，请直接输入或点击「获取模型」"}
            </div>
          )}
          <div className="border-t border-slate-800 px-3 py-1.5 text-[10px] text-slate-500">
            {options.length ? `共 ${options.length} 个候选，也可直接输入列表以外的模型名` : "可直接输入任意模型名"}
          </div>
        </div>
      )}

      {status?.text && (
        <p className={`mt-1 text-[10px] ${status.error ? "text-rose-300" : "text-emerald-300"}`}>
          {status.text}
        </p>
      )}
      {hint && <p className="mt-1 text-[10px] text-slate-500">{hint}</p>}
    </div>
  );
}
