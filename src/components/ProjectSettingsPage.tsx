import { confirmAction } from "../lib/confirm";
import { useEffect, useMemo, useState } from "react";
import { FolderCog, Save, X } from "lucide-react";
import { ART_STYLES, ASPECT_RATIOS, STORY_STYLES, applyProjectProfile } from "../lib/projectProfile";
import { IMAGE_MODELS, VIDEO_MODELS } from "../lib/models";
import { listVideoModelCapabilities, listVideoModels, type VideoModelCapability } from "../lib/ipc";
import { useProjectStore, type Project } from "../store/useProjectStore";

const inputCls =
  "w-full rounded-lg border border-slate-600 bg-slate-950/70 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-indigo-400 focus:ring-1 focus:ring-indigo-400/40";

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <div className="mb-1 text-xs font-medium text-slate-300">{label}</div>
      {children}
    </label>
  );
}

function preferredResolution(values: string[]): string {
  return values.includes("720p") ? "720p" : values[0] ?? "720p";
}

export default function ProjectSettingsPage({
  open,
  project,
  onClose,
}: {
  open: boolean;
  project: Project | null;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState<Project | null>(project);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [videoModels, setVideoModels] = useState<string[]>(VIDEO_MODELS);
  const [videoCapabilities, setVideoCapabilities] = useState<VideoModelCapability[]>([]);
  const [videoCapabilityNotice, setVideoCapabilityNotice] = useState<string | null>(null);
  const [videoCapabilityLoading, setVideoCapabilityLoading] = useState(false);

  useEffect(() => {
    setDraft(project);
    setError(null);
    setDirty(false);
    setVideoCapabilityNotice(null);
  }, [project, open]);

  useEffect(() => {
    if (!open) return;
    let disposed = false;
    setVideoCapabilityLoading(true);
    void Promise.allSettled([listVideoModels(), listVideoModelCapabilities()])
      .then(([modelsResult, capabilitiesResult]) => {
        if (disposed) return;
        if (modelsResult.status === "fulfilled") setVideoModels(modelsResult.value.length ? modelsResult.value : VIDEO_MODELS);
        if (capabilitiesResult.status === "fulfilled") setVideoCapabilities(capabilitiesResult.value);
        const failures = [
          modelsResult.status === "rejected" ? `模型目录：${String(modelsResult.reason)}` : "",
          capabilitiesResult.status === "rejected" ? `能力定义：${String(capabilitiesResult.reason)}` : "",
        ].filter(Boolean);
        if (failures.length) setVideoCapabilityNotice(`暂时无法完整读取视频模型信息：${failures.join("；")}。当前值会保留，生成前仍会再次校验。`);
      })
      .finally(() => {
        if (!disposed) setVideoCapabilityLoading(false);
      });
    return () => { disposed = true; };
  }, [open]);

  const selectedVideoCapability = useMemo(
    () => videoCapabilities.find((item) => item.id === draft?.videoModel),
    [draft?.videoModel, videoCapabilities],
  );
  const resolutionOptions = selectedVideoCapability?.resolutions.length
    ? selectedVideoCapability.resolutions
    : draft?.videoResolution ? [draft.videoResolution] : ["720p"];
  const videoModelOptions = [...new Set([draft?.videoModel, ...videoModels].filter((value): value is string => Boolean(value)))];

  useEffect(() => {
    if (!draft || !selectedVideoCapability?.resolutions.length || selectedVideoCapability.resolutions.includes(draft.videoResolution)) return;
    const nextResolution = preferredResolution(selectedVideoCapability.resolutions);
    const previousResolution = draft.videoResolution;
    setDraft((current) => current ? { ...current, videoResolution: nextResolution } : current);
    setDirty(true);
    setVideoCapabilityNotice(`${selectedVideoCapability.label} 不支持 ${previousResolution}，已切换为 ${nextResolution}。保存后会用于质检和每个视频任务。`);
  }, [draft, selectedVideoCapability]);

  if (!open || !draft) return null;

  const update = (patch: Partial<Project>) => {
    setDirty(true);
    setDraft((current) => current ? { ...current, ...patch } : current);
  };
  const attemptClose = async () => {
    if (dirty && !(await confirmAction("项目生产档案有未保存修改，确定关闭吗？"))) return;
    onClose();
  };
  const save = async () => {
    if (!draft.name.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await useProjectStore.getState().update(draft.id, { ...draft, name: draft.name.trim() });
      applyProjectProfile(draft);
      setDirty(false);
      onClose();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };
  const selectVideoModel = (videoModel: string) => {
    const capability = videoCapabilities.find((item) => item.id === videoModel);
    const currentResolution = draft.videoResolution || "720p";
    const nextResolution = capability?.resolutions.length && !capability.resolutions.includes(currentResolution)
      ? preferredResolution(capability.resolutions)
      : currentResolution;
    update({ videoModel, videoResolution: nextResolution });
    setVideoCapabilityNotice(nextResolution !== currentResolution
      ? `${capability?.label ?? videoModel} 不支持 ${currentResolution}，已切换为 ${nextResolution}。`
      : null);
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-[#020617]/80 p-4 backdrop-blur-md" onClick={attemptClose}>
      <section className="dream-dialog w-full max-w-2xl rounded-2xl border border-cyan-200/15 bg-slate-950/90" onClick={(e) => e.stopPropagation()}>
        <header className="flex items-center gap-2 border-b border-slate-800 px-5 py-4">
          <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-indigo-500"><FolderCog size={16} /></span>
          <div>
            <div className="text-sm font-semibold">项目生产档案</div>
            <div className="text-[10px] text-slate-500">保存后同步到生成参数，并注入文本 Agent 上下文</div>
          </div>
          <button onClick={attemptClose} className="ml-auto rounded-md p-1 text-slate-400 hover:bg-slate-800 hover:text-white"><X size={16} /></button>
        </header>

        <div className="grid max-h-[70vh] grid-cols-2 gap-4 overflow-y-auto p-5">
          <Field label="项目名称">
            <input className={inputCls} value={draft.name} onChange={(e) => update({ name: e.target.value })} />
          </Field>
          <Field label="题材类型">
            <select className={inputCls} value={draft.storyStyle} onChange={(e) => update({ storyStyle: e.target.value })}>
              {STORY_STYLES.map((value) => <option key={value}>{value}</option>)}
            </select>
          </Field>
          <div className="col-span-2">
            <Field label="项目简介">
              <textarea className={`${inputCls} h-20 resize-y`} value={draft.description} onChange={(e) => update({ description: e.target.value })} placeholder="故事主题、受众、核心冲突或制作要求" />
            </Field>
          </div>
          <Field label="视觉风格">
            <select className={inputCls} value={draft.artStyle} onChange={(e) => update({ artStyle: e.target.value })}>
              {ART_STYLES.map((value) => <option key={value}>{value}</option>)}
            </select>
          </Field>
          <Field label="统一画幅">
            <select className={inputCls} value={draft.aspectRatio} onChange={(e) => update({ aspectRatio: e.target.value })}>
              {ASPECT_RATIOS.map((value) => <option key={value}>{value}</option>)}
            </select>
          </Field>
          <Field label="默认图像模型">
            <select className={inputCls} value={draft.imageModel} onChange={(e) => update({ imageModel: e.target.value })}>
              {(draft.imageModel && !IMAGE_MODELS.includes(draft.imageModel) ? [draft.imageModel, ...IMAGE_MODELS] : IMAGE_MODELS).map((value) => <option key={value}>{value}</option>)}
            </select>
          </Field>
          <Field label="默认图像质量">
            <select className={inputCls} value={draft.imageQuality} onChange={(e) => update({ imageQuality: e.target.value })}>
              {["high", "medium", "low"].map((value) => <option key={value}>{value}</option>)}
            </select>
          </Field>
          <div className="col-span-2">
            <Field label="默认视频模型">
              <select className={inputCls} value={draft.videoModel} onChange={(e) => selectVideoModel(e.target.value)}>
                {videoModelOptions.map((value) => <option key={value} value={value}>{videoCapabilities.find((item) => item.id === value)?.label ?? value}</option>)}
              </select>
            </Field>
            <Field label="默认视频分辨率">
              <select
                className={inputCls}
                value={draft.videoResolution || resolutionOptions[0]}
                disabled={videoCapabilityLoading || !selectedVideoCapability}
                onChange={(e) => { update({ videoResolution: e.target.value }); setVideoCapabilityNotice(null); }}
              >
                {resolutionOptions.map((value) => <option key={value}>{value}</option>)}
              </select>
            </Field>
            <p className="mt-2 text-[10px] leading-relaxed text-slate-500">
              {videoCapabilityLoading
                ? "正在读取当前 API Key 可用模型及分辨率…"
                : selectedVideoCapability
                  ? `${selectedVideoCapability.label} 支持：${selectedVideoCapability.resolutions.join(" / ")}。分辨率会作为独立 API 参数提交，不依赖 Prompt。`
                  : "尚未获得该模型的分辨率能力，暂不允许盲选其他值；视频生成前会由网关再次校验。"}
            </p>
            {videoCapabilityNotice && <p role="status" className="mt-2 rounded-lg border border-amber-300/15 bg-amber-300/5 px-3 py-2 text-xs text-amber-100">{videoCapabilityNotice}</p>}
          </div>
        </div>

        {error && <div className="mx-5 mb-3 rounded-lg bg-rose-500/10 px-3 py-2 text-xs text-rose-300">保存失败：{error}</div>}

        <footer className="flex justify-end gap-2 border-t border-slate-800 px-5 py-3">
          <button onClick={attemptClose} className="rounded-lg border border-slate-600 px-4 py-2 text-xs text-slate-300 hover:bg-slate-800">取消</button>
          <button onClick={save} disabled={busy || !draft.name.trim()} className="flex items-center gap-1.5 rounded-lg bg-indigo-500 px-4 py-2 text-xs font-medium text-white hover:bg-indigo-400 disabled:opacity-50">
            <Save size={13} /> {busy ? "保存中…" : "保存并应用"}
          </button>
        </footer>
      </section>
    </div>
  );
}
