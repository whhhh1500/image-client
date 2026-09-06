import { useEffect } from "react";
import { BookOpenText, History, X } from "lucide-react";

export type AppDocsView = "guide" | "changelog";

const sectionClass = "space-y-2 rounded-xl border border-white/5 bg-slate-950/35 p-4";
const headingClass = "text-xs font-semibold text-slate-100";
const copyClass = "text-[12px] leading-6 text-slate-400";

function GuideContent() {
  return (
    <div className="space-y-3">
      <section className={sectionClass}>
        <h3 className={headingClass}>快速开始</h3>
        <ol className={`${copyClass} list-decimal space-y-1 pl-4`}>
          <li>在顶部选择项目；首次使用可新建项目并填写生产档案。</li>
          <li>打开右上角“设置”，配置图像、视频和文本模型接口。</li>
          <li>选择功能页，输入提示词或导入素材后开始生成。</li>
          <li>生成结果会进入资产库，可再次打开或作为参考图使用。</li>
        </ol>
      </section>

      <section className={sectionClass}>
        <h3 className={headingClass}>图像与视频</h3>
        <ul className={`${copyClass} list-disc space-y-1 pl-4`}>
          <li>图像生成：填写画面描述，按需选择尺寸、质量和参考图。</li>
          <li>视频生成：填写动作与镜头要求，可使用已有图片作为首帧参考。</li>
          <li>提交后请等待任务完成；切换页面不会删除已保存的历史结果。</li>
          <li>提示词应明确主体、环境、构图和风格，避免互相冲突的要求。</li>
        </ul>
      </section>

      <section className={sectionClass}>
        <h3 className={headingClass}>小说漫画</h3>
        <ol className={`${copyClass} list-decimal space-y-1 pl-4`}>
          <li>导入或创建 Markdown 小说，先保存正文并完成作品分析。</li>
          <li>依次检查作品设定、本章剧本、分镜和每页 Prompt。</li>
          <li>确认内容后生成漫画；单页重画只影响当前页本次请求。</li>
          <li>采用历史版本前先核对版本号，避免覆盖尚未保存的草稿。</li>
        </ol>
      </section>

      <section className={sectionClass}>
        <h3 className={headingClass}>资产与项目</h3>
        <p className={copyClass}>
          资产库集中显示图像与视频历史。项目用于隔离生产设置与内容；切换项目后，请确认当前项目名称再继续生成。删除项目不会自动删除本地资产文件。
        </p>
      </section>

      <section className={sectionClass}>
        <h3 className={headingClass}>常见问题</h3>
        <dl className={`${copyClass} space-y-2`}>
          <div><dt className="text-slate-300">显示“未配置”</dt><dd>打开设置，检查接口地址、密钥与模型名称并保存。</dd></div>
          <div><dt className="text-slate-300">任务失败</dt><dd>先查看错误提示，再点击底部“日志”打开日志目录定位原因。</dd></div>
          <div><dt className="text-slate-300">历史没有更新</dt><dd>点击顶部“刷新历史”，重新从本地数据库载入结果。</dd></div>
          <div><dt className="text-slate-300">安全提示</dt><dd>不要在截图、文档或反馈中公开 API 密钥与本地隐私素材。</dd></div>
        </dl>
      </section>
    </div>
  );
}

function ChangelogContent() {
  return (
    <div className="space-y-3">
      <section className={sectionClass}>
        <div className="flex flex-wrap items-baseline justify-between gap-2">
          <h3 className="text-sm font-semibold text-slate-100">v0.1.0</h3>
          <time dateTime="2026-09-06" className="text-[11px] text-slate-500">2026-09-06</time>
        </div>
        <div className="space-y-3 pt-1">
          <div>
            <h4 className="text-[11px] font-medium text-cyan-100/80">新增</h4>
            <ul className={`${copyClass} mt-1 list-disc space-y-1 pl-4`}>
              <li>图像与视频生成、参考素材和本地历史资产管理。</li>
              <li>小说 Markdown 分析、章节编排、漫画生产与单页重画。</li>
              <li>通用短剧 Agent 流水线和多项目生产档案。</li>
              <li>应用内使用文档与版本更新入口。</li>
            </ul>
          </div>
          <div>
            <h4 className="text-[11px] font-medium text-cyan-100/80">改进</h4>
            <ul className={`${copyClass} mt-1 list-disc space-y-1 pl-4`}>
              <li>生成历史可自动刷新，也可手动从数据库重新载入。</li>
              <li>生成结果保留来源信息，便于确认使用的模型与输入。</li>
              <li>失败状态与日志入口更集中，便于本地排查。</li>
            </ul>
          </div>
        </div>
      </section>
    </div>
  );
}

export default function AppDocsDialog({ view, onClose }: { view: AppDocsView | null; onClose: () => void }) {
  useEffect(() => {
    if (!view) return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [view, onClose]);

  if (!view) return null;

  const isGuide = view === "guide";
  const title = isGuide ? "使用文档" : "更新日志";

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4"
      onClick={(event) => { if (event.currentTarget === event.target) onClose(); }}
    >
      <section
        role="dialog"
        aria-modal="true"
        aria-labelledby="app-docs-title"
        className="dream-dialog flex max-h-[84vh] w-full max-w-2xl flex-col overflow-hidden rounded-2xl border border-slate-700 bg-slate-900 shadow-2xl"
      >
        <header className="flex items-center justify-between border-b border-white/5 px-5 py-4">
          <div className="flex items-center gap-2.5">
            <span className="flex h-8 w-8 items-center justify-center rounded-lg border border-cyan-200/10 bg-cyan-200/5 text-cyan-100/80">
              {isGuide ? <BookOpenText size={16} /> : <History size={16} />}
            </span>
            <div>
              <h2 id="app-docs-title" className="text-sm font-semibold text-slate-100">{title}</h2>
              <p className="mt-0.5 text-[10px] text-slate-500">{isGuide ? "Image-Client 简明操作说明" : "版本能力与重要变更"}</p>
            </div>
          </div>
          <button
            type="button"
            autoFocus
            aria-label={`关闭${title}`}
            onClick={onClose}
            className="rounded-lg p-2 text-slate-500 hover:bg-slate-800 hover:text-slate-100"
          >
            <X size={16} />
          </button>
        </header>
        <div className="min-h-0 overflow-y-auto p-5">
          {isGuide ? <GuideContent /> : <ChangelogContent />}
        </div>
      </section>
    </div>
  );
}
