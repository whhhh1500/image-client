import { confirmAction } from "../lib/confirm";
import { useEffect, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  FolderOpen,
  KeyRound,
  Loader2,
  Plus,
  Save,
  Settings2,
  Star,
  Trash2,
  X,
} from "lucide-react";
import type { ConfigStatus } from "../lib/ipc";
import { listVideoModels } from "../lib/ipc";
import { IMAGE_MODELS, LLM_MODELS } from "../lib/models";
import {
  emptyProfile,
  loadSettings,
  saveSettings,
  type ConfigProfile,
} from "../lib/settings";
import { logEvent } from "../lib/logger";

const inputCls =
  "w-full rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-indigo-400 focus:ring-1 focus:ring-indigo-400/40";

function ConnForm({
  tab,
  conn,
  models,
  onUpdate,
  ready,
}: {
  tab: "image" | "video";
  conn: { url: string; key: string; model: string };
  models: string[];
  onUpdate: (patch: Partial<{ url: string; key: string; model: string }>) => void;
  ready: boolean;
}) {
  const options = conn.model && !models.includes(conn.model) ? [conn.model, ...models] : models;
  return (
    <div className="space-y-4">
      <label className="block">
        <div className="mb-1 text-xs font-medium text-slate-300">
          {tab === "image" ? "图像" : "视频"} API 地址
        </div>
        <input
          className={inputCls}
          value={conn.url}
          placeholder={tab === "image" ? "…/v1/images/generations" : "…/v1/videos"}
          onChange={(e) => onUpdate({ url: e.target.value })}
          spellCheck={false}
        />
      </label>
      <label className="block">
        <div className="mb-1 text-xs font-medium text-slate-300">API Key</div>
        <input
          type="password"
          className={inputCls}
          value={conn.key}
          placeholder={ready ? "已配置（留空保持不变）" : "sk-…"}
          onChange={(e) => onUpdate({ key: e.target.value })}
          autoComplete="off"
        />
      </label>
      <label className="block">
        <div className="mb-1 flex items-center justify-between text-xs font-medium text-slate-300">
          <span>{tab === "image" ? "图像" : "视频"}模型</span>
          <span className="text-[10px] text-slate-500">单选</span>
        </div>
        <select
          className={inputCls}
          value={conn.model}
          onChange={(e) => onUpdate({ model: e.target.value })}
        >
          {options.length ? (
            options.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))
          ) : (
            <option value={conn.model}>{conn.model}</option>
          )}
        </select>
      </label>
    </div>
  );
}

function GlobalSettingsForm({
  outputDir,
  setOutputDir,
  llmUrl,
  setLlmUrl,
  llmKey,
  setLlmKey,
  llmModel,
  setLlmModel,
  status,
  setDirty,
}: {
  outputDir: string;
  setOutputDir: (v: string) => void;
  llmUrl: string;
  setLlmUrl: (v: string) => void;
  llmKey: string;
  setLlmKey: (v: string) => void;
  llmModel: string;
  setLlmModel: (v: string) => void;
  status: ConfigStatus | null;
  setDirty: (d: boolean) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label className="block">
          <div className="mb-1 flex items-center justify-between text-xs font-medium text-slate-300">
            <span>产物输出目录</span>
            <span className="text-[10px] text-slate-500">
              按 类别/任务标签 分层存放
            </span>
          </div>
          <div className="flex gap-2">
            <input
              className={inputCls}
              value={outputDir}
              placeholder="C:\Users\...\ImageClient\assets"
              onChange={(e) => { setOutputDir(e.target.value); setDirty(true); }}
              spellCheck={false}
            />
            <button
              type="button"
              onClick={async () => {
                const dir = await openDialog({ directory: true, multiple: false });
                if (typeof dir === "string") { setOutputDir(dir); setDirty(true); }
              }}
              className="flex shrink-0 items-center gap-1 rounded-lg border border-slate-600 px-3 py-2 text-xs text-slate-200 transition hover:bg-slate-800"
            >
              <FolderOpen size={13} /> 浏览
            </button>
          </div>
        </label>
        <p className="mt-1 text-[10px] text-slate-500">
          留空使用默认：主目录\ImageClient\assets
        </p>
      </div>

      <div className="border-t border-slate-800 pt-4 space-y-3">
        <div className="text-[11px] font-semibold uppercase tracking-wide text-emerald-300">文本 LLM（生产 Agent）</div>
        <label className="block">
          <div className="mb-1 text-xs font-medium text-slate-300">LLM API 地址</div>
          <input className={inputCls} value={llmUrl} onChange={(e) => { setLlmUrl(e.target.value); setDirty(true); }} placeholder="https://…/v1" spellCheck={false} />
        </label>
        <label className="block">
          <div className="mb-1 text-xs font-medium text-slate-300">LLM API Key</div>
          <input type="password" className={inputCls} value={llmKey} onChange={(e) => { setLlmKey(e.target.value); setDirty(true); }} placeholder={status?.llmReady ? "已配置（留空保持不变）" : "sk-…"} autoComplete="off" />
        </label>
        <label className="block">
          <div className="mb-1 text-xs font-medium text-slate-300">LLM 模型</div>
          <select className={inputCls} value={llmModel} onChange={(e) => { setLlmModel(e.target.value); setDirty(true); }}>
            {(llmModel && !LLM_MODELS.includes(llmModel) ? [llmModel, ...LLM_MODELS] : LLM_MODELS).map((m) => (<option key={m} value={m}>{m}</option>))}
          </select>
        </label>
      </div>
    </div>
  );
}

export default function SettingsPage({
  open,
  onClose,
  onSaved,
  status,
}: {
  open: boolean;
  onClose: () => void;
  onSaved: (s: ConfigStatus) => void;
  status: ConfigStatus | null;
}) {
  const [configs, setConfigs] = useState<ConfigProfile[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [tab, setTab] = useState<"image" | "video">("image");
  const [videoModels, setVideoModels] = useState<string[]>([]);
  const [outputDir, setOutputDir] = useState("");
  const [llmUrl, setLlmUrl] = useState("");
  const [llmKey, setLlmKey] = useState("");
  const [llmModel, setLlmModel] = useState("gemini-3.7-flash");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);

  useEffect(() => {
    if (!open) return;
    setMessage(null);
    setDirty(false);
    setOutputDir("");
    setLlmUrl("");
    setLlmModel(status?.llmModel ?? "gemini-3.7-flash");
    setLlmKey("");
    loadSettings().then((s) => {
      setConfigs(s.configs);
      setActiveId(s.activeId);
      setSelectedId(s.activeId ?? s.configs[0]?.id ?? "global");
      setOutputDir(s.outputDir ?? "");
      if (s.llmUrl) setLlmUrl(s.llmUrl);
      if (s.llmModel) setLlmModel(s.llmModel);
      setDirty(false);
    });
    listVideoModels().then(setVideoModels).catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  useEffect(() => {
    if (selectedId === null) {
      setSelectedId(configs[0]?.id ?? "global");
    }
  }, [configs, selectedId]);

  if (!open) return null;

  const isGlobalSelected = selectedId === "global" || (selectedId === null && configs.length === 0);
  const selected = isGlobalSelected ? null : configs.find((c) => c.id === selectedId) ?? null;

  const persist = async (nextActiveId = activeId) => {
    setBusy(true);
    setMessage(null);
    try {
      const st = await saveSettings({ configs, activeId: nextActiveId, outputDir, llmUrl, llmKey, llmModel });
      if (st) onSaved(st);
      setMessage("已保存");
      setDirty(false);
    } catch (e) {
      setMessage(`保存失败: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  const addConfig = () => {
    const next = emptyProfile(configs.length + 1);
    setConfigs((prev) => [...prev, next]);
    setSelectedId(next.id);
    setTab("image");
    setMessage(null);
    setDirty(true);
  };

  const removeConfig = async (id: string) => {
    const current = configs.find((config) => config.id === id);
    if (!(await confirmAction(`确定删除接口配置“${current?.name ?? "未命名"}”吗？保存设置后生效。`))) return;
    const next = configs.filter((c) => c.id !== id);
    setConfigs(next);
    if (activeId === id) setActiveId(next[0]?.id ?? null);
    if (selectedId === id) setSelectedId(next[0]?.id ?? "global");
    setDirty(true);
  };

  const setDefault = async (id: string) => {
    setActiveId(id);
    setSelectedId(id);
    await persist(id);
  };

  const updateSelected = (patch: { name?: string; image?: Partial<{ url: string; key: string; model: string }>; video?: Partial<{ url: string; key: string; model: string }> }) => {
    setDirty(true);
    setConfigs((prev) =>
      prev.map((c) =>
        c.id === selectedId
          ? {
              ...c,
              name: patch.name ?? c.name,
              image: { ...c.image, ...(patch.image ?? {}) },
              video: { ...c.video, ...(patch.video ?? {}) },
            }
          : c,
      ),
    );
  };

  const attemptClose = async () => {
    if (dirty && !(await confirmAction("接口配置有未保存修改，确定关闭吗？"))) return;
    onClose();
  };

  return (
    <div className="fixed inset-0 z-50 flex flex-col bg-[#020617]/80 backdrop-blur-md" onClick={attemptClose}>
      <div className="flex h-full w-full p-6">
        <div className="dream-dialog mx-auto flex h-full w-full max-w-4xl overflow-hidden rounded-2xl border border-cyan-200/15 bg-slate-950/90" onClick={(event) => event.stopPropagation()}>
          {/* Left: config list */}
          <aside className="flex w-72 shrink-0 flex-col border-r border-slate-800 bg-slate-900/60">
            {/* Top: Global settings tab */}
            <div className="p-3 pb-2">
              <button
                type="button"
                onClick={() => {
                  setSelectedId("global");
                  setMessage(null);
                }}
                className={`flex w-full cursor-pointer items-center justify-between rounded-lg border px-3 py-2 text-left text-xs transition ${
                  isGlobalSelected
                    ? "border-emerald-400 bg-slate-800"
                    : "border-slate-700/60 hover:border-slate-500"
                }`}
              >
                <div className="flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded bg-emerald-500/20 text-emerald-300">
                    <Settings2 size={13} />
                  </span>
                  <div>
                    <div className="font-medium text-slate-100">全局通用设置</div>
                    <div className="text-[10px] text-slate-400">产物目录 · 文本 LLM</div>
                  </div>
                </div>
              </button>
            </div>

            <div className="flex items-center justify-between border-t border-slate-800/80 px-4 py-2.5">
              <div className="flex items-center gap-2">
                <span className="flex h-6 w-6 items-center justify-center rounded bg-indigo-500/20 text-indigo-300">
                  <KeyRound size={13} />
                </span>
                <div className="text-xs font-semibold text-slate-300">接口配置 Profiles</div>
              </div>
              <button
                type="button"
                onClick={addConfig}
                title="新增配置"
                className="flex h-6 w-6 items-center justify-center rounded border border-slate-600 text-slate-200 transition hover:bg-slate-800"
              >
                <Plus size={13} />
              </button>
            </div>
            <div className="min-h-0 flex-1 space-y-1.5 overflow-y-auto p-3 pt-0">
              {configs.length === 0 && (
                <div className="rounded-lg border border-dashed border-slate-700 p-4 text-center text-xs text-slate-500">
                  暂无接口配置
                  <button
                    type="button"
                    onClick={addConfig}
                    className="mt-2 flex w-full items-center justify-center gap-1 rounded-lg bg-indigo-500 px-2 py-1.5 text-xs font-medium text-white hover:bg-indigo-400"
                  >
                    <Plus size={13} /> 新增配置
                  </button>
                </div>
              )}
              {configs.map((c) => (
                <div
                  key={c.id}
                  onClick={() => {
                    setSelectedId(c.id);
                    setMessage(null);
                  }}
                  className={`flex cursor-pointer items-center justify-between rounded-lg border px-2.5 py-2 text-xs transition ${
                    !isGlobalSelected && c.id === selectedId
                      ? "border-indigo-400 bg-slate-800"
                      : "border-slate-700/60 hover:border-slate-500"
                  }`}
                >
                  <div className="min-w-0">
                    <div className="flex items-center gap-1">
                      {c.id === activeId && (
                        <Star size={11} className="text-amber-400" fill="currentColor" />
                      )}
                      <span className="truncate font-medium text-slate-100">{c.name}</span>
                    </div>
                    <div className="truncate text-[10px] text-slate-500">{c.image.model}</div>
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    {c.id === activeId && (
                      <span className="rounded bg-amber-400/10 px-1 py-0.5 text-[9px] text-amber-300">
                        默认
                      </span>
                    )}
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        removeConfig(c.id);
                      }}
                      className="rounded p-1 text-slate-500 hover:text-rose-300"
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </aside>

          {/* Right: edit form */}
          <section className="flex min-w-0 flex-1 flex-col">
            <div className="flex items-center justify-between border-b border-slate-800 px-5 py-3">
              <div className="text-sm font-semibold">
                {isGlobalSelected ? "全局通用设置" : (selected ? selected.name : "选择或新增配置")}
              </div>
              <button
                type="button"
                onClick={attemptClose}
                className="rounded-md p-1 text-slate-400 hover:bg-slate-800 hover:text-white"
              >
                <X size={16} />
              </button>
            </div>

            {isGlobalSelected ? (
              <div className="min-h-0 flex-1 overflow-y-auto p-5">
                <div className="mb-4 text-xs text-slate-400">
                  全局通用配置项，所有生成任务与剧本/分镜 Agent 共享。
                </div>
                <GlobalSettingsForm
                  outputDir={outputDir}
                  setOutputDir={setOutputDir}
                  llmUrl={llmUrl}
                  setLlmUrl={setLlmUrl}
                  llmKey={llmKey}
                  setLlmKey={setLlmKey}
                  llmModel={llmModel}
                  setLlmModel={setLlmModel}
                  status={status}
                  setDirty={setDirty}
                />
                {message && (
                  <div className="mt-4 text-xs text-slate-400">{message}</div>
                )}
              </div>
            ) : selected ? (
              <div className="min-h-0 flex-1 overflow-y-auto p-5">
                <label className="mb-4 block">
                  <div className="mb-1 text-xs font-medium text-slate-300">配置名称</div>
                  <input
                    className={inputCls}
                    value={selected.name}
                    onChange={(e) => updateSelected({ name: e.target.value })}
                  />
                </label>

                <div className="mb-4 flex gap-1 rounded-lg bg-slate-800/70 p-1">
                  {(["image", "video"] as const).map((t) => (
                    <button
                      key={t}
                      type="button"
                      onClick={() => setTab(t)}
                      className={`flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition ${
                        tab === t ? "bg-indigo-500 text-white" : "text-slate-400 hover:text-white"
                      }`}
                    >
                      {t === "image" ? "图像" : "视频"}
                    </button>
                  ))}
                </div>

                {tab === "image" ? (
                  <ConnForm
                    tab="image"
                    conn={selected.image}
                    models={IMAGE_MODELS}
                    onUpdate={(p) => updateSelected({ image: p })}
                    ready={!selected.image.key}
                  />
                ) : (
                  <ConnForm
                    tab="video"
                    conn={selected.video}
                    models={videoModels}
                    onUpdate={(p) => updateSelected({ video: p })}
                    ready={!selected.video.key}
                  />
                )}

                <div className="mt-6 border-t border-slate-800 pt-5">
                  <div className="mb-3 flex items-center justify-between">
                    <div className="text-xs font-semibold text-slate-300">全局通用设置（共享）</div>
                    <button
                      type="button"
                      onClick={() => setSelectedId("global")}
                      className="text-[11px] text-emerald-400 hover:underline"
                    >
                      切换到完整全局设置
                    </button>
                  </div>
                  <GlobalSettingsForm
                    outputDir={outputDir}
                    setOutputDir={setOutputDir}
                    llmUrl={llmUrl}
                    setLlmUrl={setLlmUrl}
                    llmKey={llmKey}
                    setLlmKey={setLlmKey}
                    llmModel={llmModel}
                    setLlmModel={setLlmModel}
                    status={status}
                    setDirty={setDirty}
                  />
                </div>

                {message && (
                  <div className="mt-4 text-xs text-slate-400">{message}</div>
                )}
              </div>
            ) : (
              <div className="flex flex-1 items-center justify-center text-sm text-slate-500">
                点左侧「+」新增一个接口配置，或切换到「全局通用设置」
              </div>
            )}

            <div className="flex items-center justify-between border-t border-slate-800 px-5 py-3">
              <button
                type="button"
                onClick={() => selectedId && !isGlobalSelected && void setDefault(selectedId).catch((error) => logEvent("error", "settings.default_profile_failed", { error: String(error) }))}
                disabled={!selected || isGlobalSelected}
                className="flex items-center gap-1.5 rounded-lg border border-slate-600 px-3 py-2 text-xs font-medium text-slate-200 transition hover:bg-slate-800 disabled:opacity-50"
              >
                <Star size={13} /> 设为默认
              </button>
              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={attemptClose}
                  className="rounded-lg border border-slate-600 px-4 py-2 text-xs font-medium text-slate-300 transition hover:bg-slate-800"
                >
                  取消
                </button>
                <button
                  type="button"
                  onClick={() => void persist()}
                  disabled={busy}
                  className="flex items-center gap-1.5 rounded-lg bg-indigo-500 px-4 py-2 text-xs font-medium text-white transition hover:bg-indigo-400 disabled:opacity-50"
                >
                  {busy ? <Loader2 size={14} className="animate-spin" /> : <Save size={14} />}
                  保存配置
                </button>
              </div>
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
