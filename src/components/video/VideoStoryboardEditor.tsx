import { Copy, Plus, Trash2 } from "lucide-react";
import { createEmptyStoryboardShot, resequenceStoryboardShots, type StoryboardShot, type VideoReferenceStrategy } from "../../lib/video/storyboard";

const input = "w-full rounded-lg border border-slate-600/80 bg-slate-950/65 px-3 py-2 text-xs text-slate-100 outline-none focus:border-violet-300/70";

function list(value: string): string[] {
  return value.split(/[,，、\n]/).map((item) => item.trim()).filter(Boolean);
}

function shown(value: unknown): string {
  return Array.isArray(value) ? value.join("、") : String(value ?? "");
}

const labels: Record<keyof StoryboardShot, string> = {
  shotNo: "镜号", scene: "场次", shotType: "景别", composition: "构图", light: "光线", camera: "运镜",
  action: "画面动作", emotion: "情绪", durationS: "时长", startState: "起始状态", actionProgress: "动作过程",
  endState: "结束状态", continuityFrom: "承接镜头", styleAnchor: "画风锚", sceneAnchor: "场景锚",
  characterAnchors: "角色锚", propAnchors: "道具锚", referenceStrategy: "参考方式", referenceAssetIds: "参考资产",
  sourceDialogue: "来源对白", videoPrompt: "视频 Prompt",
};

const displayOrder: Array<keyof StoryboardShot> = [
  "scene", "durationS", "shotType", "composition", "light", "camera", "action", "emotion",
  "startState", "actionProgress", "endState", "continuityFrom", "styleAnchor", "sceneAnchor",
  "characterAnchors", "propAnchors", "referenceStrategy", "referenceAssetIds", "sourceDialogue", "videoPrompt",
];

export default function VideoStoryboardEditor({ shots, editable, onChange }: { shots: StoryboardShot[]; editable: boolean; onChange: (shots: StoryboardShot[]) => void }) {
  const update = (index: number, patch: Partial<StoryboardShot>) => onChange(shots.map((shot, current) => current === index ? { ...shot, ...patch } : shot));
  const text = (index: number, key: keyof StoryboardShot, value: string, rows = 2) => <textarea rows={rows} value={value} onChange={(event) => update(index, { [key]: event.target.value } as Partial<StoryboardShot>)} className={`${input} resize-y`} />;
  const labeled = (label: string, child: React.ReactNode, wide = false) => <label className={wide ? "md:col-span-2" : ""}><span className="mb-1 block text-[10px] text-slate-500">{label}</span>{child}</label>;

  return <div className="grid gap-3 [grid-template-columns:repeat(auto-fit,minmax(320px,1fr))]">
    {shots.map((shot, index) => <article key={`${shot.shotNo}-${index}`} className="rounded-xl border border-violet-300/15 bg-slate-950/45 p-3">
      <div className="mb-3 flex items-center gap-2">
        <span className="rounded-full border border-violet-300/20 bg-violet-300/10 px-2 py-0.5 text-[10px] font-semibold text-violet-200">第 {shot.shotNo} 镜 · {shot.durationS} 秒</span>
        <span className="min-w-0 flex-1 truncate text-xs text-slate-300">{shot.scene} · {shot.shotType}</span>
        {editable && <div className="flex gap-1">
          <button aria-label={`复制第 ${shot.shotNo} 镜`} title="复制镜头" onClick={() => onChange(resequenceStoryboardShots([...shots.slice(0, index + 1), { ...shot }, ...shots.slice(index + 1)]))} className="rounded p-1 text-slate-400 hover:text-cyan-200"><Copy size={12} /></button>
          <button aria-label={`删除第 ${shot.shotNo} 镜`} title="删除镜头" onClick={() => onChange(resequenceStoryboardShots(shots.filter((_, current) => current !== index)))} className="rounded p-1 text-slate-400 hover:text-rose-300"><Trash2 size={12} /></button>
        </div>}
      </div>
      {editable ? <div className="grid gap-2 md:grid-cols-2">
        {labeled("镜号", <input value={shot.shotNo} disabled className={`${input} cursor-not-allowed opacity-60`} />)}
        {labeled("时长（秒）", <input type="number" min={1} value={shot.durationS} onChange={(event) => update(index, { durationS: Math.max(1, Number(event.target.value) || 1) })} className={input} />)}
        {labeled("场次", <input value={shot.scene} onChange={(event) => update(index, { scene: event.target.value })} className={input} />, true)}
        {labeled("景别", <input value={shot.shotType} onChange={(event) => update(index, { shotType: event.target.value })} className={input} />)}
        {labeled("情绪", <input value={shot.emotion} onChange={(event) => update(index, { emotion: event.target.value })} className={input} />)}
        {labeled("构图", text(index, "composition", shot.composition), true)}
        {labeled("光线", text(index, "light", shot.light), true)}
        {labeled("运镜", text(index, "camera", shot.camera), true)}
        {labeled("画面动作", text(index, "action", shot.action, 3), true)}
        {labeled("起始状态", text(index, "startState", shot.startState), true)}
        {labeled("动作过程", text(index, "actionProgress", shot.actionProgress), true)}
        {labeled("结束状态", text(index, "endState", shot.endState), true)}
        {labeled("承接镜头", <input value={shot.continuityFrom === null ? "" : shot.continuityFrom} placeholder="无" onChange={(event) => update(index, { continuityFrom: event.target.value ? Math.max(1, Number(event.target.value)) : null })} className={input} />)}
        {labeled("参考方式", <select value={shot.referenceStrategy} onChange={(event) => update(index, { referenceStrategy: event.target.value as VideoReferenceStrategy })} className={input}><option value="text">text</option><option value="first_frame">first_frame</option><option value="reference">reference</option></select>)}
        {labeled("画风锚", <input value={shot.styleAnchor} onChange={(event) => update(index, { styleAnchor: event.target.value })} className={input} />)}
        {labeled("场景锚", <input value={shot.sceneAnchor} onChange={(event) => update(index, { sceneAnchor: event.target.value })} className={input} />)}
        {(["characterAnchors", "propAnchors", "referenceAssetIds"] as const).map((key) => labeled(labels[key], <input value={shot[key].join("、")} onChange={(event) => update(index, { [key]: list(event.target.value) } as Partial<StoryboardShot>)} className={input} />, true))}
        {labeled("来源对白（不进入视频 Prompt）", text(index, "sourceDialogue", shot.sourceDialogue), true)}
        {labeled("视频 Prompt", text(index, "videoPrompt", shot.videoPrompt, 4), true)}
      </div> : <div className="grid gap-2 md:grid-cols-2">{displayOrder.map((key) => <div key={key} className={`${["composition", "light", "camera", "action", "startState", "actionProgress", "endState", "characterAnchors", "propAnchors", "referenceAssetIds", "sourceDialogue", "videoPrompt"].includes(key) ? "md:col-span-2" : ""} rounded-lg border border-white/5 bg-white/[0.025] p-2`}><div className="text-[9px] text-slate-600">{labels[key]}</div><div className="mt-1 whitespace-pre-wrap text-[11px] text-slate-300">{key === "durationS" ? `${shot.durationS} 秒` : key === "continuityFrom" ? (shot.continuityFrom === null ? "无" : `第${shot.continuityFrom}镜`) : shown(shot[key]) || "无"}</div></div>)}</div>}
    </article>)}
    {editable && <button onClick={() => onChange([...shots, createEmptyStoryboardShot(shots)])} className="flex min-h-32 items-center justify-center gap-2 rounded-xl border border-dashed border-violet-300/20 text-xs text-violet-200 hover:bg-violet-300/5"><Plus size={14} /> 新增镜头</button>}
  </div>;
}
