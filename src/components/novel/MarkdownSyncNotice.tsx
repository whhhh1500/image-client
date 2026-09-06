import { mdLabels, type MdScope, type MdWorkspace } from "../../lib/comic/markdownApi";
import { button, primary, syncDraftBlock } from "./mdWorkspaceState";
import { useEffect, useReducer } from "react";

export default function MarkdownSyncNotice({ scope, workspace, disabled, onSync, onChooseChapter }: {
  scope: MdScope; workspace: MdWorkspace; disabled: boolean; onSync: () => Promise<void>; onChooseChapter: (chapterId: string) => void;
}) {
  const [, refreshDrafts] = useReducer((value: number) => value + 1, 0);
  useEffect(() => { window.addEventListener("comic-md-draft-change", refreshDrafts); return () => window.removeEventListener("comic-md-draft-change", refreshDrafts); }, []);
  const plan = workspace.syncPlan;
  const others = (workspace.affectedChapters ?? []).filter((chapter) => chapter.chapterId !== scope.chapterId);
  const affected = (plan?.targets.length ?? 0) + (plan?.missingPageNos.length ?? 0);
  const obsolete = plan?.obsoletePageNos ?? [];
  if (!affected && !obsolete.length && !others.length && !plan?.blockedReason) return null;
  const blocked = syncDraftBlock(scope, workspace);
  return <section aria-label="需要联动更新" className="space-y-2 rounded-lg border border-amber-400/25 bg-amber-500/5 p-3 text-sm">
    {affected > 0 && <>
      <div className="flex flex-wrap items-center justify-between gap-2"><p className="font-medium text-amber-100">需要联动更新 · {plan!.targets.length} 份文字{plan!.missingPageNos.length > 0 && ` + ${plan!.missingPageNos.length} 页待补`}</p><button className={primary} disabled={disabled || !!blocked} onClick={() => void onSync()}>更新本章受影响文字</button></div>
      <p className="text-slate-400">依据已保存内容，按设定、剧本、分镜、页 Prompt 的顺序更新受影响文字。可能调用文本模型计费，不会自动生成图片。</p>
      <details className="text-slate-300"><summary className="cursor-pointer">查看受影响内容与原因</summary><ul className="mt-2 space-y-1">{plan!.targets.map((target) => <li key={target.documentId}>{target.kind === "page_prompt" ? `第${target.pageNo}页 Prompt` : mdLabels[target.kind]}：{target.reasons.join("；") || "依赖内容已有变化"}</li>)}{plan!.missingPageNos.map((pageNo) => <li key={`missing-${pageNo}`}>第{pageNo}页 Prompt：当前分镜已安排，尚待补齐</li>)}</ul></details>
    </>}
    {blocked && <p role="alert" className="text-amber-200">{blocked}</p>}
    {obsolete.length > 0 && <p className="text-slate-400">第{obsolete.join("、")}页不在当前分镜中，旧文字和图片保留，可查看、复制和查阅历史；不参与本章批量出图与默认导出。</p>}
    {others.length > 0 && <details className="text-slate-300"><summary className="cursor-pointer">本书另有 {others.length} 章需要更新</summary><ul className="mt-2 space-y-2">{others.map((chapter) => <li key={chapter.chapterId} className="flex flex-wrap items-center gap-2"><span>第{chapter.chapterNo}章 · {chapter.title || "未命名"} · {chapter.documentCount}份文字：{chapter.reason}</span><button className={button} onClick={() => onChooseChapter(chapter.chapterId)}>前往第{chapter.chapterNo}章处理</button></li>)}</ul></details>}
  </section>;
}
