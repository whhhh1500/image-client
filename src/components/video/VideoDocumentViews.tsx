import type { StoryboardShot } from "../../lib/video/storyboard";
import VideoStoryboardEditor from "./VideoStoryboardEditor";

export type VideoDocumentView = "overview" | "markdown" | "compare";

const tab = "rounded-lg border px-3 py-1.5 text-xs transition";
const input = "w-full rounded-xl border border-slate-700 bg-slate-950/60 px-4 py-3 font-mono text-sm leading-7 text-slate-100 outline-none focus:border-cyan-300/50";

function MarkdownPreview({ markdown }: { markdown: string }) {
  return <article aria-label="Markdown 阅读预览" className="min-h-[56vh] space-y-2 overflow-auto rounded-xl border border-slate-800 bg-slate-950/35 p-6 text-sm leading-7 text-slate-200">{markdown.split("\n").map((line, index) => {
    const heading = /^(#{1,6})\s+(.+)$/.exec(line);
    if (heading) return <div key={index} role="heading" aria-level={heading[1].length} className={heading[1].length === 1 ? "pb-3 pt-3 text-xl font-semibold" : "pt-4 text-base font-semibold text-cyan-50"}>{heading[2]}</div>;
    return <p key={index} className="whitespace-pre-wrap break-words">{line || "\u00a0"}</p>;
  })}</article>;
}

function AnchorOverview({ markdown }: { markdown: string }) {
  const sections: Array<{ heading: string; entries: string[] }> = [];
  let active: { heading: string; entries: string[] } | undefined;
  for (const line of markdown.split(/\r?\n/)) {
    const heading = /^(#{2,3})\s+(.+)$/.exec(line);
    if (heading) {
      active = { heading: heading[2].trim(), entries: [] };
      sections.push(active);
    } else if (active && line.trim() && !line.startsWith("#")) active.entries.push(line.trim());
  }
  return <div aria-label="锚点结构化总览" className="grid min-h-[56vh] content-start gap-3 md:grid-cols-2">{sections.length ? sections.map((section, index) => <section key={`${section.heading}-${index}`} className="rounded-xl border border-violet-300/15 bg-slate-950/35 p-4"><h3 className="text-sm font-semibold text-violet-100">{section.heading}</h3><div className="mt-3 space-y-2">{section.entries.length ? section.entries.map((entry, entryIndex) => {
    const [id, ...description] = entry.split("|");
    return <div key={entryIndex} className="rounded-lg border border-white/5 bg-white/[0.02] p-3"><div className="break-all font-mono text-[11px] text-cyan-200">{id.trim()}</div><div className="mt-1 text-[11px] leading-5 text-slate-400">{description.join("|").trim() || "未填写说明"}</div></div>;
  }) : <p className="text-xs text-slate-500">本区暂无条目</p>}</div></section>) : <div className="flex min-h-52 items-center justify-center rounded-xl border border-dashed border-slate-800 text-sm text-slate-500">当前锚点 Markdown 尚无可识别分区，请切换到 Markdown 源稿编辑。</div>}</div>;
}

export default function VideoDocumentViews({ view, onViewChange, stage, stageLabel, text, baseline, editable, storyboardShots, storyboardRoundTripSafe, onTextChange, onStoryboardChange }: {
  view: VideoDocumentView;
  onViewChange: (view: VideoDocumentView) => void;
  stage: string;
  stageLabel: string;
  text: string;
  baseline: string;
  editable: boolean;
  storyboardShots: StoryboardShot[];
  storyboardRoundTripSafe: boolean;
  onTextChange: (text: string) => void;
  onStoryboardChange: (shots: StoryboardShot[]) => void;
}) {
  const hasStructuredOverview = stage === "anchors" || stage === "storyboard";
  return <div>
    <div className="mb-3 flex flex-wrap items-center gap-2" role="tablist" aria-label="文档视图">
      <button role="tab" aria-selected={view === "overview"} onClick={() => onViewChange("overview")} className={`${tab} ${view === "overview" ? "border-cyan-300/40 bg-cyan-300/10 text-cyan-100" : "border-slate-700 text-slate-400"}`}>{hasStructuredOverview ? "结构化总览" : "阅读预览"}</button>
      <button role="tab" aria-selected={view === "markdown"} onClick={() => onViewChange("markdown")} className={`${tab} ${view === "markdown" ? "border-cyan-300/40 bg-cyan-300/10 text-cyan-100" : "border-slate-700 text-slate-400"}`}>Markdown 源稿</button>
      <button role="tab" aria-selected={view === "compare"} disabled={!baseline} onClick={() => onViewChange("compare")} className={`${tab} ${view === "compare" ? "border-cyan-300/40 bg-cyan-300/10 text-cyan-100" : "border-slate-700 text-slate-400"} disabled:opacity-40`}>并排审阅</button>
      <span className="ml-auto text-[10px] text-slate-500">{text.length.toLocaleString()} 字符</span>
    </div>
    {view === "markdown" && <textarea aria-label={`${stageLabel} Markdown`} readOnly={!editable} value={text} onChange={(event) => onTextChange(event.target.value)} className={`${input} min-h-[62vh] resize-y ${editable ? "" : "cursor-not-allowed opacity-75"}`} />}
    {view === "compare" && <div className="grid gap-3 lg:grid-cols-2"><label className="text-xs text-slate-400">保存基线<textarea aria-label="保存基线" readOnly value={baseline} className={`${input} mt-2 min-h-[58vh] resize-y opacity-75`} /></label><label className="text-xs text-cyan-100">当前草稿<textarea aria-label="当前草稿" readOnly={!editable} value={text} onChange={(event) => onTextChange(event.target.value)} className={`${input} mt-2 min-h-[58vh] resize-y ${editable ? "" : "cursor-not-allowed opacity-75"}`} /></label></div>}
    {view === "overview" && stage === "anchors" && <AnchorOverview markdown={text} />}
    {view === "overview" && stage === "storyboard" && (storyboardShots.length
      ? <><VideoStoryboardEditor shots={storyboardShots} editable={editable && storyboardRoundTripSafe} onChange={onStoryboardChange} />{!storyboardRoundTripSafe && <p role="alert" className="mt-3 rounded-lg border border-amber-300/15 bg-amber-300/5 p-3 text-xs text-amber-200">源稿包含结构化编辑器不能无损保留的内容。当前总览只读，请在 Markdown 源稿中修改。</p>}</>
      : <div className="flex min-h-52 items-center justify-center rounded-xl border border-dashed border-slate-800 text-sm text-slate-500">分镜结构无效或尚未生成，请切换到 Markdown 源稿。</div>)}
    {view === "overview" && stage !== "anchors" && stage !== "storyboard" && <MarkdownPreview markdown={text} />}
  </div>;
}
