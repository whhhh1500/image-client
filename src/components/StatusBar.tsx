import { BookOpenText, History } from "lucide-react";
import type { AppInfo, ConfigStatus, ProviderInfo } from "../lib/ipc";
import { logsDir } from "../lib/ipc";
import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import { logEvent } from "../lib/logger";

export const ZZONE_INVITE_URL = "https://oenai.cc.cd/j/i1.cmVzZWxsZXIAZmQxeVp2TEY1V3hJb0JWeDltNDYwYlpS.cn5C6uhqhbNTFeR46uZf1TPAP-3Uf7-zsIcsDZovdK8";

function item(ok: boolean, label: string, extra?: string) {
  return (
    <div className="flex items-center gap-1.5">
      <span className={`h-2 w-2 rounded-full ${ok ? "bg-emerald-500" : "bg-rose-500"}`} />
      <span>
        {label}
        {extra ? <span className="text-slate-500"> {extra}</span> : null}
      </span>
    </div>
  );
}

export default function StatusBar({
  appInfo,
  dbReady,
  configStatus,
  providers,
  onOpenSettings,
  onOpenGuide,
  onOpenChangelog,
}: {
  appInfo: AppInfo | null;
  dbReady: boolean;
  configStatus: ConfigStatus | null;
  providers: ProviderInfo[] | null;
  onOpenSettings: () => void;
  onOpenGuide: () => void;
  onOpenChangelog: () => void;
}) {
  const activeProvider = providers?.find((p) => p.active);
  const activeName = activeProvider?.name ?? "";
  return (
    <footer className="flex items-center gap-5 overflow-x-auto border-t border-slate-800 bg-slate-900/70 px-4 py-1.5 text-[11px] text-slate-400">
      {item(!!appInfo, "后端", appInfo ? `v${appInfo.version}` : "连接中…")}
      {item(true, "媒体处理", "内置")}
      {item(dbReady, "数据库", dbReady ? "就绪" : "初始化中…")}
      <button
        onClick={onOpenSettings}
        className={`flex items-center gap-1.5 ${configStatus?.imageReady ? "text-slate-400" : "text-amber-300"}`}
      >
        <span className={`h-2 w-2 rounded-full ${configStatus?.imageReady ? "bg-emerald-500" : "bg-rose-500"}`} />
        图像 {configStatus?.imageReady ? `${configStatus.imageModel}` : "未配置"}
        <span className="text-[10px] text-slate-600">配置</span>
      </button>
      <button
        onClick={() => void logsDir()
          .then((path) => openPath(path))
          .catch((error) => logEvent("error", "logs.open_failed", { error: String(error) }))}
        className="text-slate-500 transition hover:text-slate-200"
        title="打开与数据库同级的日志目录"
      >
        日志
      </button>
      <div className="ml-auto flex shrink-0 items-center gap-3">
        {activeProvider?.id === "zzone" ? <button type="button" onClick={() => void openUrl(ZZONE_INVITE_URL).catch((error) => logEvent("error", "provider.invite_open_failed", { providerId: "zzone", error: String(error) }))} className="max-w-40 truncate text-slate-500 transition hover:text-cyan-200" title="用默认浏览器打开 ZZone 网关邀请页面">{activeName}</button> : activeName ? <span className="max-w-40 truncate text-slate-500">{activeName}</span> : null}
        {activeName && <span className="h-3 w-px bg-slate-700/70" aria-hidden="true" />}
        <button type="button" onClick={onOpenGuide} className="flex items-center gap-1 text-slate-500 hover:text-slate-200">
          <BookOpenText size={12} /> 使用文档
        </button>
        <button type="button" onClick={onOpenChangelog} className="flex items-center gap-1 text-slate-500 hover:text-slate-200">
          <History size={12} /> 更新日志
        </button>
      </div>
    </footer>
  );
}
