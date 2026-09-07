export type VideoStageStatus = "not_started" | "draft" | "partial" | "current" | "stale" | "blocked" | "review_required" | "needs_changes";

const STATUS_LABELS: Record<VideoStageStatus, string> = {
  not_started: "未开始",
  draft: "草稿未保存",
  partial: "部分完成",
  current: "当前",
  stale: "需更新",
  blocked: "被阻塞",
  review_required: "待审查",
  needs_changes: "需修改",
};

const STATUS_STYLES: Record<VideoStageStatus, string> = {
  not_started: "text-slate-500",
  draft: "text-cyan-200",
  partial: "text-sky-300",
  current: "text-emerald-300",
  stale: "text-amber-300",
  blocked: "text-rose-300",
  review_required: "text-violet-300",
  needs_changes: "text-orange-300",
};

export function videoResultStageStatus(totalShots: number, completedShots: number, failedTasks: number, productionReady: boolean): VideoStageStatus {
  if (!productionReady) return "blocked";
  if (totalShots > 0 && completedShots === totalShots) return "current";
  if (completedShots > 0 || failedTasks > 0) return "partial";
  return "not_started";
}

export default function VideoStageNavigation<T extends string>({ items, current, onSelect, idPrefix = "video-stage" }: {
  items: Array<{ id: T; label: string; status: VideoStageStatus; detail?: string }>;
  current: T;
  onSelect: (id: T) => void;
  idPrefix?: string;
}) {
  const tabRefs = new Map<T, HTMLButtonElement | null>();
  const activate = (index: number) => {
    const next = items[index];
    if (!next) return;
    tabRefs.get(next.id)?.focus();
    onSelect(next.id);
  };
  const onTabKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>, index: number) => {
    let nextIndex: number | undefined;
    if (event.key === "ArrowRight") nextIndex = (index + 1) % items.length;
    if (event.key === "ArrowLeft") nextIndex = (index - 1 + items.length) % items.length;
    if (event.key === "Home") nextIndex = 0;
    if (event.key === "End") nextIndex = items.length - 1;
    if (nextIndex === undefined) return;
    event.preventDefault();
    activate(nextIndex);
  };

  return <nav aria-label="短剧生产阶段" className="mt-3">
    <div role="tablist" aria-label="短剧生产阶段" aria-orientation="horizontal" className="flex gap-2 overflow-x-auto pb-1">
      {items.map((item, index) => {
        const selected = item.id === current;
        const tabId = `${idPrefix}-tab-${item.id}`;
        const panelId = `${idPrefix}-panel-${item.id}`;
        return <button
          key={item.id}
          ref={(element) => { tabRefs.set(item.id, element); }}
          id={tabId}
          role="tab"
          aria-selected={selected}
          aria-controls={panelId}
          tabIndex={selected ? 0 : -1}
          onClick={() => onSelect(item.id)}
          onKeyDown={(event) => onTabKeyDown(event, index)}
          className={`min-w-32 rounded-xl border px-3 py-2 text-left transition ${selected ? "border-cyan-300/40 bg-cyan-300/10" : "border-slate-800 bg-slate-950/30 hover:border-slate-600"}`}
        >
          <span className="flex items-center gap-2 text-xs font-medium text-slate-200"><span className="flex h-5 w-5 items-center justify-center rounded-full border border-white/10 text-[10px] text-slate-400">{index + 1}</span>{item.label}</span>
          <span className={`mt-1 block pl-7 text-[10px] ${STATUS_STYLES[item.status]}`}>{item.detail ?? STATUS_LABELS[item.status]}</span>
        </button>;
      })}
    </div>
  </nav>;
}
