import { confirmAction } from "../lib/confirm";
import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { agentLabel, agentSystem, useAgentStore, AGENT_DEFAULTS, type AgentVersion } from "../store/useAgentStore";
import { logEvent } from "../lib/logger";

// zustand v5 + React 19 要求 selector 返回稳定引用；
// 内联 `?? []` 每次产生新数组会导致 useSyncExternalStore 无限重渲染（#185）。
const EMPTY_VERSIONS: AgentVersion[] = [];

export default function PromptManager({
  open,
  onClose,
  initialAgent,
}: {
  open: boolean;
  onClose: () => void;
  initialAgent: string;
}) {
  const [agentId, setAgentId] = useState(initialAgent);
  const [ver, setVer] = useState<number | "builtin">("builtin");
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);

  const versions = useAgentStore((s) => s.versions[agentId] ?? EMPTY_VERSIONS);
  // 订阅整个 versions 记录，左侧列表的版本计数才会随保存/回滚刷新；
  // 不能在渲染里用 getState()（不会触发重渲染）。
  const allVersions = useAgentStore((s) => s.versions);
  const list = [...versions].sort((a, b) => b.v - a.v);
  const activeV = list.find((x) => x.enabled)?.v ?? null;

  useEffect(() => {
    if (!open) return;
    setMessage(null);
    setAgentId(initialAgent);
    const values = useAgentStore.getState().versions[initialAgent] ?? [];
    const active = [...values].sort((a, b) => b.v - a.v).find((item) => item.enabled);
    setVer(active?.v ?? "builtin");
    setDraft(active?.system ?? agentSystem(initialAgent));
    setDirty(false);
  }, [initialAgent, open]);

  if (!open) return null;

  const loadAgent = async (id: string) => {
    if (dirty && !(await confirmAction("切换 Agent 会丢失当前未保存 Prompt，确定继续吗？"))) return;
    setAgentId(id);
    const vs = useAgentStore.getState().versions[id] ?? [];
    const used = [...vs].sort((a, b) => b.v - a.v).find((x) => x.enabled);
    if (used) { setVer(used.v); setDraft(used.system); }
    else { setVer("builtin"); setDraft(agentSystem(id)); }
    setDirty(false);
  };

  const loadVer = async (v: number | "builtin") => {
    if (dirty && !(await confirmAction("切换版本会丢失当前未保存 Prompt，确定继续吗？"))) return;
    setVer(v);
    if (v === "builtin") setDraft(agentSystem(agentId));
    else { const x = versions.find((y) => y.v === v); if (x) setDraft(x.system); }
    setDirty(false);
  };

  const saveAsVersion = async () => {
    if (!draft.trim() || busy) return;
    setBusy(true);
    setMessage(null);
    try {
      await useAgentStore.getState().addVersion(agentId, draft);
      const latest = Math.max(...(useAgentStore.getState().versions[agentId] ?? []).map((item) => item.v), 1);
      setVer(latest);
      setDirty(false);
      setMessage(`已保存为 v${latest}，并立即作为当前 Prompt 使用。`);
    } catch (error) {
      setMessage(`保存失败：${error}`);
      logEvent("error", "agent_prompt.save_failed", { error: String(error) });
    } finally {
      setBusy(false);
    }
  };

  const attemptClose = async () => {
    if (dirty && !(await confirmAction("Prompt 有未保存修改，确定关闭吗？"))) return;
    onClose();
  };

  const toggleVersion = async (version: AgentVersion) => {
    setMessage(null);
    try {
      await useAgentStore.getState().setEnabled(agentId, version.v, !version.enabled);
      setMessage(`v${version.v} 已${version.enabled ? "停用" : "启用"}。${version.enabled ? "如果没有其他启用版本，将回落到内置默认。" : "它会参与最新启用版本选择。"}`);
    } catch (error) {
      setMessage(`操作失败：${error}`);
      logEvent("error", "agent_prompt.toggle_failed", { error: String(error) });
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex bg-[#020617]/80 backdrop-blur-md" onClick={attemptClose}>
      <div className="flex h-full w-full p-6">
        <div className="dream-dialog mx-auto flex h-full w-full max-w-5xl overflow-hidden rounded-2xl border border-cyan-200/15 bg-slate-950/90" onClick={(event) => event.stopPropagation()}>
          {/* Left: six agents */}
          <aside className="flex w-56 shrink-0 flex-col border-r border-slate-800 bg-slate-900/60">
            <div className="px-4 py-3 text-sm font-semibold">Prompt 管理</div>
            <div className="min-h-0 flex-1 space-y-1.5 overflow-y-auto p-3">
              {AGENT_DEFAULTS.map((a) => (
                <button
                  key={a.id}
                  onClick={() => loadAgent(a.id)}
                  className={`w-full rounded-lg border px-3 py-2 text-left text-sm transition ${a.id === agentId ? "border-indigo-400 bg-slate-800" : "border-slate-700/60 hover:border-slate-500"}`}
                >
                  <span className="font-medium text-slate-100">{a.label}</span>
                  <span className="ml-1 text-[10px] text-slate-500">{(allVersions[a.id] ?? EMPTY_VERSIONS).length} 版本</span>
                </button>
              ))}
            </div>
            <p className="border-t border-slate-800 p-3 text-[11px] text-slate-500">
              未保存版本时用内置默认。新增版本后：取最新启用版本，全停用则回落内置。
            </p>
          </aside>

          {/* Right: editor */}
          <section className="flex min-w-0 flex-1 flex-col">
            <div className="flex items-center justify-between border-b border-slate-800 px-5 py-3">
              <div className="text-sm font-semibold">{agentLabel(agentId)} · 版本切换</div>
              <button onClick={attemptClose} className="rounded-md p-1 text-slate-400 hover:bg-slate-800 hover:text-white"><X size={18} /></button>
            </div>

            <div className="min-h-0 flex-1 overflow-y-auto p-5">
              <div className="mb-3 flex flex-wrap items-center gap-3">
                <label className="text-xs font-medium text-slate-300">版本</label>
                <select
                  value={String(ver)}
                  onChange={(e) => loadVer(e.target.value === "builtin" ? "builtin" : Number(e.target.value))}
                  className="rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-1.5 text-xs text-slate-100 outline-none focus:border-indigo-400"
                >
                  <option value="builtin">内置默认{activeV === null ? "（当前）" : ""}</option>
                  {list.map((x) => (
                    <option key={x.v} value={String(x.v)}>
                      v{x.v}{x.enabled ? "（启用中）" : "（停用）"}
                    </option>
                  ))}
                </select>
                <div className="flex gap-2">
                  <button onClick={() => void saveAsVersion()} disabled={!draft.trim() || busy} className="rounded-lg bg-indigo-500 px-3 py-1.5 text-xs font-medium text-white hover:bg-indigo-400 disabled:opacity-50">
                    {busy ? "保存中…" : "+ 存为新版本"}
                  </button>
                  <button onClick={() => { setDraft(agentSystem(agentId)); setDirty(true); }} className="rounded-lg border border-slate-600 px-3 py-1.5 text-xs text-slate-200 hover:bg-slate-800">
                    重置
                  </button>
                </div>
              </div>

              {message && <div className="mb-3 rounded-lg border border-cyan-300/10 bg-cyan-300/[0.035] px-3 py-2 text-xs text-cyan-100">{message}</div>}

              <textarea
                className="h-72 w-full resize-y rounded-xl border border-slate-700 bg-slate-950/60 p-4 font-mono text-sm leading-relaxed text-slate-200 outline-none focus:border-indigo-400"
                value={draft}
                onChange={(e) => { setDraft(e.target.value); setDirty(true); }}
              />

              <div className="mt-4">
                <div className="mb-2 text-xs font-semibold text-slate-400">版本列表（启用/停用）</div>
                <div className="space-y-1.5">
                  {list.length === 0 && <div className="text-xs text-slate-500">暂无版本，当前用内置默认。</div>}
                  {list.map((x) => (
                    <div key={x.v} className="flex items-center justify-between rounded-lg border border-slate-700/60 px-3 py-2 text-xs">
                      <div className="min-w-0">
                        <span className="font-medium text-slate-100">v{x.v}</span>
                        {x.enabled && <span className="ml-2 text-indigo-300">启用中</span>}
                        <div className="truncate text-[10px] text-slate-500">{x.system.split("\n")[0]}</div>
                      </div>
                      <button
                        onClick={() => void toggleVersion(x)}
                        className={`shrink-0 rounded px-2 py-1 text-[11px] ${x.enabled ? "text-amber-300 hover:bg-slate-800" : "text-emerald-300 hover:bg-slate-800"}`}
                      >
                        {x.enabled ? "停用" : "启用"}
                      </button>
                    </div>
                  ))}
                </div>
              </div>
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
