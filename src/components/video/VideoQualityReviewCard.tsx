import { Loader2, SearchCheck, Sparkles } from "lucide-react";
import type { VideoQualityReviewStatus } from "../../lib/video/qualityReview";
import { parseVideoQualityReview } from "../../lib/video/qualityReview";

const button = "rounded-lg border border-slate-700 px-3 py-2 text-xs text-slate-200 hover:border-cyan-300/30 disabled:cursor-not-allowed disabled:opacity-40";

export default function VideoQualityReviewCard({ review, status, score, busy, disabled, onReview, onUseSuggestions }: {
  review?: string;
  status?: VideoQualityReviewStatus;
  score?: number | null;
  busy: boolean;
  disabled?: boolean;
  onReview: () => void;
  onUseSuggestions: () => void;
}) {
  let parsed: ReturnType<typeof parseVideoQualityReview> | undefined;
  try { parsed = review ? parseVideoQualityReview(review) : undefined; } catch { parsed = undefined; }
  const blockerPreview = parsed?.blockingIssues && !/^(无|无。)$/.test(parsed.blockingIssues)
    ? parsed.blockingIssues.split(/\r?\n/).map((line) => line.trim()).filter(Boolean).slice(0, 3)
    : [];
  const label = !review ? "待审查" : status === "passed" ? "通过" : "需修改";
  const tone = !review ? "border-violet-400/15 bg-violet-400/5 text-violet-100" : status === "passed" ? "border-emerald-400/15 bg-emerald-400/5 text-emerald-100" : "border-orange-400/20 bg-orange-400/5 text-orange-100";
  return <section aria-label="产物质量审查" className={`rounded-xl border p-4 ${tone}`}>
    <div className="flex items-center justify-between gap-3">
      <div><div className="text-xs font-semibold">独立质量审查</div><div className="mt-1 text-[11px] opacity-75">{label}{typeof score === "number" ? ` · ${score} 分` : ""}</div></div>
      <button className={button} disabled={busy || disabled} onClick={onReview}>{busy ? <Loader2 size={13} className="mr-1 inline animate-spin" /> : <SearchCheck size={13} className="mr-1 inline" />}{review ? "重新审查" : "审查当前草稿"}</button>
    </div>
    {!review ? <p className="mt-3 text-[11px] leading-5 text-slate-400">检查原文忠实度、剧情吸引力、模板腔、人物与情感、物理可信度、画幅、锚点和视频可执行性。会额外调用一次文本模型。</p> : <>
      {blockerPreview.length > 0 && <div className="mt-3 rounded-lg border border-orange-300/15 bg-orange-300/5 p-3"><div className="text-[10px] font-semibold text-orange-100">优先修复</div><ul className="mt-2 space-y-1 text-[11px] leading-5 text-orange-100/80">{blockerPreview.map((line) => <li key={line}>• {line}</li>)}</ul></div>}
      <details className="mt-3 rounded-lg border border-white/5 bg-slate-950/25 p-3">
        <summary className="cursor-pointer text-[11px] text-slate-300">查看完整审查报告</summary>
        <pre className="mt-3 max-h-72 overflow-auto whitespace-pre-wrap font-sans text-[11px] leading-5 text-slate-300">{review}</pre>
      </details>
      {status !== "passed" && <><details className="mt-3 rounded-lg border border-white/5 p-2 text-[11px] text-slate-400"><summary className="cursor-pointer">预览将带入的优化建议</summary><pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap font-sans leading-5">{parsed?.suggestions || parsed?.blockingIssues || "根据审查报告修复问题"}</pre></details><button className={`${button} mt-2 w-full`} onClick={onUseSuggestions}><Sparkles size={13} className="mr-1 inline" />把审查建议带入优化要求</button></>}
    </>}
  </section>;
}
