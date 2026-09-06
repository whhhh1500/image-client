import { useEffect, useMemo, useState } from "react";
import { Loader2, RotateCcw, Search, Sparkles, X } from "lucide-react";
import CachedCaseImage from "./CachedCaseImage";
import { hydrateCaseImageCache, importCaseImage } from "../lib/caseImage";
import { logEvent } from "../lib/logger";
import { LLM_MODELS } from "../lib/models";
import { optimizeImagePrompt } from "../lib/optimizePrompt";
import {
  applyPlaceholders,
  catalog,
  categoryLabel,
  effectivePrompt,
  extractPlaceholders,
  isModified,
  searchEntries,
  type PromptlibEntry,
  type PromptlibKind,
} from "../lib/promptlib";
import { usePromptlibStore } from "../store/usePromptlibStore";

const inputCls =
  "w-full rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-indigo-400 focus:ring-1 focus:ring-indigo-400/40";

export interface PromptLibraryPick {
  prompt: string;
  entry: PromptlibEntry;
  asReference?: boolean;
  referencePath?: string;
}

export default function PromptLibrary({
  open,
  onClose,
  onPick,
  llmModel,
}: {
  open: boolean;
  onClose: () => void;
  onPick: (pick: PromptLibraryPick) => void;
  llmModel?: string;
}) {
  const overrides = usePromptlibStore((s) => s.overrides);
  const loaded = usePromptlibStore((s) => s.loaded);
  const [kind, setKind] = useState<PromptlibKind | "all">("case");
  const [category, setCategory] = useState("");
  const [query, setQuery] = useState("");
  const [modifiedOnly, setModifiedOnly] = useState(false);
  const [selected, setSelected] = useState<PromptlibEntry | null>(null);
  const [draft, setDraft] = useState("");
  const [jsonDraft, setJsonDraft] = useState("");
  const [useJson, setUseJson] = useState(false);
  const [placeholderValues, setPlaceholderValues] = useState<Record<string, string>>({});
  const [optimizeNote, setOptimizeNote] = useState("");
  const [optimized, setOptimized] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      void usePromptlibStore.getState().load();
      void hydrateCaseImageCache();
    }
  }, [open]);

  useEffect(() => {
    if (!open) {
      setSelected(null);
      setError(null);
      setOptimized("");
      setOptimizeNote("");
      setBusy(null);
    }
  }, [open]);

  const entries = useMemo(() => {
    const source = kind === "template" ? catalog.templates : kind === "case" ? catalog.cases : [...catalog.templates, ...catalog.cases];
    return searchEntries(source, overrides, { kind: "all", category, query, modifiedOnly });
  }, [category, kind, modifiedOnly, overrides, query]);

  const placeholders = useMemo(() => extractPlaceholders(draft), [draft]);
  const filled = useMemo(() => applyPlaceholders(draft, placeholderValues), [draft, placeholderValues]);

  const openEntry = (entry: PromptlibEntry) => {
    setSelected(entry);
    setDraft(effectivePrompt(entry, overrides, "prompt"));
    setJsonDraft(effectivePrompt(entry, overrides, "jsonPrompt"));
    setUseJson(false);
    setPlaceholderValues({});
    setOptimized("");
    setOptimizeNote("");
    setError(null);
    logEvent("info", "promptlib.open_entry", { id: entry.id, kind: entry.kind });
  };

  const currentPrompt = () => (useJson && jsonDraft.trim() ? jsonDraft : filled);

  const saveEdit = async () => {
    if (!selected) return;
    setBusy("save");
    setError(null);
    try {
      await usePromptlibStore.getState().saveOverride(selected.id, draft, jsonDraft || undefined);
      setOptimized("");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const restore = async () => {
    if (!selected) return;
    setBusy("restore");
    setError(null);
    try {
      await usePromptlibStore.getState().restore(selected.id);
      setDraft(selected.prompt);
      setJsonDraft(selected.jsonPrompt ?? "");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const runOptimize = async () => {
    setBusy("optimize");
    setError(null);
    try {
      const model = llmModel && LLM_MODELS.includes(llmModel) ? llmModel : undefined;
      const result = await optimizeImagePrompt({
        prompt: currentPrompt(),
        userIntent: optimizeNote,
        guidance: selected?.guidance,
        pitfalls: selected?.pitfalls,
        model,
      });
      setOptimized(result);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const pickPrompt = (prompt: string) => {
    if (!selected) return;
    onPick({ prompt, entry: selected });
    onClose();
  };

  const pickAsReference = async () => {
    if (!selected) return;
    setBusy("reference");
    setError(null);
    try {
      const path = await importCaseImage(selected);
      onPick({ prompt: currentPrompt(), entry: selected, asReference: true, referencePath: path });
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4" onClick={onClose}>
      <div
        className="flex h-[88vh] w-full max-w-6xl overflow-hidden rounded-2xl border border-slate-700 bg-slate-900 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="flex items-center gap-3 border-b border-slate-800 px-5 py-3">
            <div>
              <div className="text-sm font-semibold">提示词模板库</div>
              <div className="text-[10px] text-slate-500">
                当前 {entries.length} 条 · {catalog.caseCount} 个案例 · {catalog.templateCount} 套工业模板 · 来源 {catalog.source.name}（{catalog.source.license}）
              </div>
            </div>
            <button type="button" onClick={onClose} className="ml-auto rounded-md p-1 text-slate-400 hover:bg-slate-800 hover:text-white">
              <X size={16} />
            </button>
          </div>

          <div className="flex flex-wrap items-center gap-2 border-b border-slate-800 px-5 py-3">
            {(["case", "template", "all"] as const).map((value) => (
              <button
                key={value}
                type="button"
                onClick={() => setKind(value)}
                className={`rounded-md px-2.5 py-1 text-[11px] ${kind === value ? "bg-indigo-500 text-white" : "bg-slate-800 text-slate-300"}`}
              >
                {value === "case" ? "案例" : value === "template" ? "工业模板" : "全部"}
              </button>
            ))}
            <label className="relative min-w-[180px] flex-1">
              <Search size={13} className="pointer-events-none absolute left-2.5 top-2.5 text-slate-500" />
              <input className={`${inputCls} pl-8`} value={query} placeholder="搜索标题、关键词、提示词… 支持 动漫=anime" onChange={(e) => setQuery(e.target.value)} />
            </label>
            <label className="flex items-center gap-1 text-[11px] text-slate-400">
              <input type="checkbox" checked={modifiedOnly} onChange={(e) => setModifiedOnly(e.target.checked)} />
              只看已修改
            </label>
          </div>

          <div className="flex gap-1 overflow-x-auto border-b border-slate-800 px-5 py-2">
            <button type="button" onClick={() => setCategory("")} className={`shrink-0 rounded-md px-2 py-1 text-[10px] ${!category ? "bg-slate-700 text-white" : "text-slate-400 hover:text-white"}`}>
              全部分类
            </button>
            {catalog.categories.map((item) => (
              <button
                key={item}
                type="button"
                onClick={() => setCategory(item)}
                className={`shrink-0 rounded-md px-2 py-1 text-[10px] ${category === item ? "bg-slate-700 text-white" : "text-slate-400 hover:text-white"}`}
              >
                {categoryLabel(item)}
              </button>
            ))}
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto p-4">
            {!loaded ? (
              <div className="flex h-40 items-center justify-center text-xs text-slate-500">加载本地修改…</div>
            ) : entries.length === 0 ? (
              <div className="flex h-40 flex-col items-center justify-center gap-1 text-xs text-slate-500">
                <div>没有匹配的模板</div>
                <div>试试 anime、插画，或先点「全部分类」</div>
              </div>
            ) : (
              <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4">
                {entries.map((entry) => (
                  <button
                    key={entry.id}
                    type="button"
                    onClick={() => openEntry(entry)}
                    className={`overflow-hidden rounded-xl border text-left transition ${selected?.id === entry.id ? "border-indigo-400" : "border-slate-700 hover:border-slate-500"}`}
                  >
                    <div className="relative aspect-[4/3] bg-slate-800">
                      <CachedCaseImage image={entry.image} alt={entry.title} className="h-full w-full object-cover" />
                      {isModified(entry.id, overrides) && (
                        <span className="absolute left-1.5 top-1.5 rounded bg-amber-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-slate-950">已修改</span>
                      )}
                      <span className="absolute bottom-1.5 right-1.5 rounded bg-black/60 px-1.5 py-0.5 text-[9px] text-slate-100">
                        {entry.kind === "template" ? "模板" : "案例"}
                      </span>
                    </div>
                    <div className="space-y-1 p-2">
                      <div className="line-clamp-2 text-xs font-medium text-slate-100">{entry.title}</div>
                      <div className="text-[10px] text-slate-500">{categoryLabel(entry.category)}</div>
                    </div>
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>

        {selected && (
          <aside className="flex w-[380px] shrink-0 flex-col border-l border-slate-800 bg-slate-950/40">
            <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-4">
              <div>
                <div className="text-sm font-semibold">{selected.title}</div>
                <div className="mt-1 text-[10px] text-slate-500">
                  {categoryLabel(selected.category)}
                  {selected.sourceLabel ? ` · ${selected.sourceLabel}` : ""}
                  {isModified(selected.id, overrides) ? " · 使用你保存的版本" : " · 内置默认"}
                </div>
              </div>
              {selected.description && <p className="text-[11px] leading-relaxed text-slate-400">{selected.description}</p>}
              {selected.useWhen && <p className="text-[11px] text-slate-400">适用：{selected.useWhen}</p>}
              {!!selected.guidance?.length && (
                <div className="text-[10px] text-emerald-200/80">建议：{selected.guidance.join("；")}</div>
              )}
              {!!selected.pitfalls?.length && (
                <div className="text-[10px] text-rose-200/80">避坑：{selected.pitfalls.join("；")}</div>
              )}

              {selected.jsonPrompt && (
                <div className="flex gap-1">
                  <button type="button" onClick={() => setUseJson(false)} className={`rounded px-2 py-1 text-[10px] ${!useJson ? "bg-slate-700 text-white" : "text-slate-400"}`}>文本模板</button>
                  <button type="button" onClick={() => setUseJson(true)} className={`rounded px-2 py-1 text-[10px] ${useJson ? "bg-slate-700 text-white" : "text-slate-400"}`}>JSON 模板</button>
                </div>
              )}

              <label className="block">
                <div className="mb-1 text-[11px] text-slate-400">模板正文（改这里才会入库）</div>
                <textarea
                  className={`${inputCls} h-40 resize-y font-mono text-[11px] leading-relaxed`}
                  value={useJson ? jsonDraft : draft}
                  onChange={(e) => (useJson ? setJsonDraft(e.target.value) : setDraft(e.target.value))}
                />
              </label>

              {placeholders.length > 0 && !useJson && (
                <div className="space-y-2 rounded-lg border border-slate-800 p-2">
                  <div className="text-[10px] font-medium text-slate-400">填写占位符（只影响本次回填）</div>
                  {placeholders.map((token) => (
                    <input
                      key={token}
                      className={inputCls}
                      placeholder={token}
                      value={placeholderValues[token] ?? ""}
                      onChange={(e) => setPlaceholderValues((prev) => ({ ...prev, [token]: e.target.value }))}
                    />
                  ))}
                </div>
              )}

              {!!selected.variants?.length && (
                <details className="rounded-lg border border-slate-800 p-2 text-[11px] text-slate-400">
                  <summary className="cursor-pointer">同分类变体 {selected.variants.length}</summary>
                  <div className="mt-2 space-y-2">
                    {selected.variants.map((variant) => (
                      <button
                        key={variant.title}
                        type="button"
                        className="block w-full rounded border border-slate-700 px-2 py-1 text-left hover:border-slate-500"
                        onClick={() => { setUseJson(false); setDraft(variant.prompt); }}
                      >
                        {variant.title}
                      </button>
                    ))}
                  </div>
                </details>
              )}

              <label className="block">
                <div className="mb-1 text-[11px] text-slate-400">补充给 AI 优化（可选）</div>
                <textarea className={`${inputCls} h-16 resize-y text-[11px]`} value={optimizeNote} placeholder="例如：改成竖版、主体换成女主角红衣…" onChange={(e) => setOptimizeNote(e.target.value)} />
              </label>

              {optimized && (
                <label className="block">
                  <div className="mb-1 text-[11px] text-emerald-300">优化结果（可再改）</div>
                  <textarea className={`${inputCls} h-28 resize-y font-mono text-[11px]`} value={optimized} onChange={(e) => setOptimized(e.target.value)} />
                </label>
              )}

              {error && <div className="rounded-lg bg-rose-500/10 px-3 py-2 text-[11px] text-rose-300">{error}</div>}
            </div>

            <div className="space-y-2 border-t border-slate-800 p-4">
              <div className="flex gap-2">
                <button type="button" disabled={!!busy} onClick={saveEdit} className="flex-1 rounded-lg border border-slate-600 px-2 py-1.5 text-[11px] text-slate-200 hover:bg-slate-800 disabled:opacity-50">
                  {busy === "save" ? "保存中…" : "保存修改"}
                </button>
                <button type="button" disabled={!!busy || !isModified(selected.id, overrides)} onClick={restore} className="rounded-lg border border-slate-600 px-2 py-1.5 text-[11px] text-slate-300 hover:bg-slate-800 disabled:opacity-50">
                  <RotateCcw size={12} className="inline" /> 恢复默认
                </button>
              </div>
              <div className="flex gap-2">
                <button type="button" disabled={!!busy} onClick={runOptimize} className="flex flex-1 items-center justify-center gap-1 rounded-lg bg-fuchsia-500 px-2 py-1.5 text-[11px] font-medium text-white hover:bg-fuchsia-400 disabled:opacity-50">
                  {busy === "optimize" ? <Loader2 size={12} className="animate-spin" /> : <Sparkles size={12} />}
                  优化成中文
                </button>
                {optimized && (
                  <button
                    type="button"
                    disabled={!!busy}
                    onClick={() => void usePromptlibStore.getState().saveOverride(selected.id, optimized, jsonDraft || undefined)
                      .then(() => { setDraft(optimized); setUseJson(false); setOptimized(""); })
                      .catch((e) => setError(`存回模板失败：${String(e)}`))}
                    className="rounded-lg border border-fuchsia-400/40 px-2 py-1.5 text-[11px] text-fuchsia-200 hover:bg-fuchsia-500/10 disabled:opacity-50"
                  >
                    存回模板
                  </button>
                )}
              </div>
              <button type="button" disabled={!!busy} onClick={() => pickPrompt(optimized || currentPrompt())} className="w-full rounded-lg bg-indigo-500 px-2 py-2 text-xs font-medium text-white hover:bg-indigo-400 disabled:opacity-50">
                填入生成框
              </button>
              <button type="button" disabled={!!busy} onClick={pickAsReference} className="w-full rounded-lg border border-slate-600 px-2 py-1.5 text-[11px] text-slate-200 hover:bg-slate-800 disabled:opacity-50">
                {busy === "reference" ? "下载原图中…" : "下载原图并设为参考图"}
              </button>
              <a href={catalog.source.url} target="_blank" rel="noreferrer" className="block text-center text-[10px] text-slate-500 hover:text-slate-300">
                模板来源 · MIT
              </a>
            </div>
          </aside>
        )}
      </div>
    </div>
  );
}
