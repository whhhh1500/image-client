export default function WorkflowGuide({
  current,
  next,
  detail,
}: {
  current: string;
  next: string;
  detail?: string;
}) {
  return (
    <div className="workflow-guide flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-white/5 px-6 py-2.5 text-[10px]">
      <span className="text-cyan-200/80">当前：{current}</span>
      <span className="text-slate-700">→</span>
      <span className="text-violet-200/75">下一步：{next}</span>
      {detail && <span className="ml-auto text-slate-600">{detail}</span>}
    </div>
  );
}
