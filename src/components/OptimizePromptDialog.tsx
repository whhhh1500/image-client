import { useEffect, useRef, useState } from "react";
import { Loader2, Sparkles, X } from "lucide-react";
import { confirmAction } from "../lib/confirm";
import { errorText } from "../lib/errors";
import { LLM_MODELS } from "../lib/models";
import { optimizeImagePrompt } from "../lib/optimizePrompt";
import { useEscapeKey } from "../lib/useEscapeKey";

const inputCls =
  "w-full rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-indigo-400 focus:ring-1 focus:ring-indigo-400/40";

export default function OptimizePromptDialog({
  open,
  getPrompt,
  guidance,
  pitfalls,
  llmModel,
  onClose,
  onAdopt,
  onSaveAsTemplate,
}: {
  open: boolean;
  /** Read once when the dialog opens; the caller must pass a stable function. */
  getPrompt: () => string;
  guidance?: string[];
  pitfalls?: string[];
  llmModel?: string;
  onClose: () => void;
  onAdopt: (prompt: string) => void;
  onSaveAsTemplate?: (prompt: string) => void | Promise<void>;
}) {
  const [source, setSource] = useState("");
  const [note, setNote] = useState("");
  const [result, setResult] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The result last stored with 存回模板; any other result is lost on close.
  const [savedResult, setSavedResult] = useState<string | null>(null);
  // Every open/close bumps the token, so a response that arrives after the
  // dialog was closed can never leak into the next session.
  const requestRef = useRef(0);

  useEffect(() => {
    requestRef.current += 1;
    setNote("");
    setResult("");
    setError(null);
    setBusy(false);
    setSavedResult(null);
    if (open) setSource(getPrompt());
  }, [open, getPrompt]);

  // Backdrop, × and Escape ask first when closing would drop an unapplied
  // result or abandon a running (paid) request; 取消 and 采用 are explicit.
  const unapplied = result.trim() !== "" && result !== savedResult;
  const attemptClose = async () => {
    const warning = busy ? "优化仍在进行，关闭后将丢弃结果，确定关闭吗？" : unapplied ? "优化结果尚未采用，确定关闭吗？" : null;
    if (warning && !(await confirmAction(warning))) return;
    onClose();
  };
  useEscapeKey(() => void attemptClose(), open);

  if (!open) return null;

  const run = async () => {
    const token = ++requestRef.current;
    setBusy(true);
    setError(null);
    try {
      const model = llmModel && LLM_MODELS.includes(llmModel) ? llmModel : undefined;
      const optimized = await optimizeImagePrompt({
        prompt: source,
        userIntent: note,
        guidance,
        pitfalls,
        model,
      });
      if (token !== requestRef.current) return;
      setResult(optimized);
    } catch (e) {
      if (token !== requestRef.current) return;
      setError(errorText(e));
    } finally {
      if (token === requestRef.current) setBusy(false);
    }
  };

  const saveAsTemplate = async (text: string) => {
    if (!onSaveAsTemplate) return;
    try {
      await onSaveAsTemplate(text);
      setSavedResult(text);
    } catch (e) {
      setError(`存回模板失败：${errorText(e)}`);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4" onClick={() => void attemptClose()}>
      <div role="dialog" aria-modal="true" aria-labelledby="optimize-prompt-title" className="w-full max-w-2xl rounded-2xl border border-slate-700 bg-slate-900 p-5 shadow-2xl" onClick={(e) => e.stopPropagation()}>
        <div className="mb-3 flex items-center justify-between">
          <div id="optimize-prompt-title" className="text-sm font-semibold">AI 优化成中文提示词</div>
          <button type="button" aria-label="关闭" onClick={() => void attemptClose()} className="rounded-md p-1 text-slate-400 hover:bg-slate-800 hover:text-white"><X size={16} /></button>
        </div>
        <div className="grid gap-3 md:grid-cols-2">
          <label className="block">
            <div className="mb-1 text-[11px] text-slate-400">当前</div>
            <pre className="h-40 overflow-auto whitespace-pre-wrap rounded-lg border border-slate-700 bg-slate-950/60 p-3 text-[11px] leading-relaxed text-slate-300">{source || "（空）"}</pre>
          </label>
          <label className="block">
            <div className="mb-1 text-[11px] text-slate-400">优化后</div>
            <textarea className={`${inputCls} h-40 resize-none font-mono text-[11px]`} value={result} placeholder="点下方按钮开始优化" onChange={(e) => setResult(e.target.value)} />
          </label>
        </div>
        <textarea autoFocus className={`${inputCls} mt-3 h-16 resize-y text-[11px]`} value={note} placeholder="补充要求（可选）：改竖版、换成夜景、保留模板结构…" onChange={(e) => setNote(e.target.value)} />
        {error && <div className="mt-3 rounded-lg bg-rose-500/10 px-3 py-2 text-[11px] text-rose-300">{error}</div>}
        <div className="mt-4 flex flex-wrap justify-end gap-2">
          <button type="button" onClick={onClose} className="rounded-lg border border-slate-600 px-3 py-1.5 text-xs text-slate-300 hover:bg-slate-800">取消</button>
          <button type="button" disabled={busy || !source.trim()} onClick={run} className="flex items-center gap-1 rounded-lg bg-fuchsia-500 px-3 py-1.5 text-xs font-medium text-white hover:bg-fuchsia-400 disabled:opacity-50">
            {busy ? <Loader2 size={13} className="animate-spin" /> : <Sparkles size={13} />}
            {result ? "再优化一次" : "开始优化"}
          </button>
          {result && onSaveAsTemplate && (
            <button type="button" onClick={() => void saveAsTemplate(result)} className="rounded-lg border border-fuchsia-400/40 px-3 py-1.5 text-xs text-fuchsia-200 hover:bg-fuchsia-500/10">
              存回模板
            </button>
          )}
          <button type="button" disabled={!result.trim()} onClick={() => { onAdopt(result); onClose(); }} className="rounded-lg bg-indigo-500 px-3 py-1.5 text-xs font-medium text-white hover:bg-indigo-400 disabled:opacity-50">
            采用
          </button>
        </div>
      </div>
    </div>
  );
}
