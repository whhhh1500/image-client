import { useEffect, useState } from "react";
import { Loader2, X } from "lucide-react";
import {
  mediaHostingGet,
  mediaHostingSave,
  type MediaHostingAuthMode,
  type MediaHostingStatus,
} from "../lib/ipc";

export interface MediaHostingDialogProps {
  open: boolean;
  onClose: () => void;
  onSaved?: (status: MediaHostingStatus) => void;
}

const inputClass = "w-full rounded-lg border border-slate-700 bg-slate-900 px-3 py-2 text-xs text-slate-100 outline-none focus:border-cyan-400";

export default function MediaHostingDialog({ open, onClose, onSaved }: MediaHostingDialogProps) {
  const [status, setStatus] = useState<MediaHostingStatus | null>(null);
  const [endpoint, setEndpoint] = useState("");
  const [fileField, setFileField] = useState("file");
  const [urlField, setUrlField] = useState("url");
  const [authMode, setAuthMode] = useState<MediaHostingAuthMode>("bearer");
  const [token, setToken] = useState("");
  const [clearToken, setClearToken] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setError(null); setToken(""); setClearToken(false);
    void mediaHostingGet().then((next) => {
      if (cancelled) return;
      setStatus(next); setEndpoint(next.endpoint); setFileField(next.fileField || "file"); setUrlField(next.urlField || "url"); setAuthMode(next.authMode);
    }).catch((cause) => !cancelled && setError(`读取媒体托管配置失败：${String(cause)}`));
    return () => { cancelled = true; };
  }, [open]);

  if (!open) return null;
  const save = async () => {
    setSaving(true); setError(null);
    try {
      const next = await mediaHostingSave({ endpoint: endpoint.trim(), fileField: fileField.trim(), urlField: urlField.trim(), authMode, token, clearToken });
      setStatus(next); setToken(""); setClearToken(false); onSaved?.(next); onClose();
    } catch (cause) {
      setError(String(cause));
    } finally { setSaving(false); }
  };
  return <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 p-4" role="dialog" aria-modal="true" aria-label="媒体托管配置">
    <section className="w-full max-w-lg rounded-2xl border border-slate-700 bg-slate-950 shadow-2xl">
      <header className="flex items-center gap-3 border-b border-slate-800 px-5 py-4"><div className="min-w-0 flex-1"><h2 className="text-sm font-semibold text-white">媒体托管配置</h2><p className="mt-1 text-[11px] text-slate-500">本地图片和视频会在你确认引用时上传到此服务，生成接口只接收返回的公网 HTTPS 地址。</p></div><button type="button" onClick={onClose} aria-label="关闭媒体托管配置" className="text-slate-400 hover:text-white"><X size={16} /></button></header>
      <div className="space-y-3 p-5">
        <label className="block text-xs text-slate-300">上传 Endpoint<input aria-label="上传 Endpoint" value={endpoint} onChange={(event) => setEndpoint(event.target.value)} placeholder="https://media.example/upload" className={`${inputClass} mt-1`} /></label>
        <div className="grid grid-cols-2 gap-3"><label className="block text-xs text-slate-300">文件字段<input aria-label="文件字段" value={fileField} onChange={(event) => setFileField(event.target.value)} className={`${inputClass} mt-1`} /></label><label className="block text-xs text-slate-300">JSON URL 字段<input aria-label="JSON URL 字段" value={urlField} onChange={(event) => setUrlField(event.target.value)} className={`${inputClass} mt-1`} /></label></div>
        <label className="block text-xs text-slate-300">令牌发送方式<select aria-label="令牌发送方式" value={authMode} onChange={(event) => setAuthMode(event.target.value as MediaHostingAuthMode)} className={`${inputClass} mt-1`}><option value="bearer">Bearer</option><option value="raw">Raw</option></select></label>
        <label className="block text-xs text-slate-300">令牌<input aria-label="媒体托管令牌" value={token} onChange={(event) => setToken(event.target.value)} type="password" placeholder={status?.hasToken ? "同一服务留空保留；更换服务需重新填写" : "可留空"} className={`${inputClass} mt-1`} /></label>
        {status?.hasToken && <label className="flex items-center gap-2 text-[11px] text-slate-400"><input type="checkbox" aria-label="清除已保存令牌" checked={clearToken} onChange={(event) => setClearToken(event.target.checked)} />清除已保存的令牌（保存后需重新填写才能使用托管上传）</label>}
        <p className="text-[10px] leading-relaxed text-slate-500">保存只写入本机配置，不会测试连接或上传文件。{status?.configured ? " 当前已有可用配置。" : " 当前未配置。"}</p>
        {error && <p className="text-xs text-rose-300">{error}</p>}
      </div>
      <footer className="flex justify-end gap-2 border-t border-slate-800 px-5 py-3"><button type="button" onClick={onClose} className="px-3 py-2 text-xs text-slate-400 hover:text-white">取消</button><button type="button" disabled={saving} onClick={() => void save()} className="flex items-center gap-1 rounded-lg bg-cyan-500 px-3 py-2 text-xs font-medium text-slate-950 disabled:opacity-50">{saving && <Loader2 size={13} className="animate-spin" />}保存配置</button></footer>
    </section>
  </div>;
}
