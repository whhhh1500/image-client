import { useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Clapperboard, FileInput, Loader2, Music, Send, X } from "lucide-react";
import { useVideoStore, type VideoParams } from "../store/useVideoStore";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import { generateVideo } from "../lib/generateVideo";
import { listVideoModels } from "../lib/ipc";
import { ContextMenu, menuIcons } from "../components/ContextMenu";
import AssetPicker from "../components/AssetPicker";
import AssetDetailModal from "../components/AssetDetailModal";
import { getDocumentMeta } from "../lib/documents";
import WorkflowGuide from "../components/WorkflowGuide";
import { ASPECT_RATIOS } from "../lib/projectProfile";
import { logEvent } from "../lib/logger";
import { snapshotAsset } from "../lib/provenance";

const inputCls =
  "w-full rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-indigo-400 focus:ring-1 focus:ring-indigo-400/40";

const DURATIONS = ["5", "10", "15", "20", "30", "45", "60"];

function Field({ label, children, hint }: { label: string; children: React.ReactNode; hint?: string }) {
  return (
    <label className="block">
      <div className="mb-1 flex items-center justify-between text-xs font-medium text-slate-300">
        <span>{label}</span>
        {hint && <span className="text-[10px] text-slate-500">{hint}</span>}
      </div>
      {children}
    </label>
  );
}

export default function VideoPanel() {
  const vid = useVideoStore();
  const [models, setModels] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; asset: LibAsset } | null>(null);
  const [playing, setPlaying] = useState<LibAsset | null>(null);
  const [guideAsset, setGuideAsset] = useState<LibAsset | null>(null);
  const [textSource, setTextSource] = useState<LibAsset | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);

  const provider = useProjectStore();
  const defaultId = provider.projects[0]?.id;
  const activeId = provider.activeId;
  const belongs = (pid?: string) => pid === activeId || (!pid && activeId === defaultId);
  const allAssets = useLibraryStore((s) => s.assets);
  const videos = allAssets.filter((a) => a.asset.kind === "video" && belongs(a.projectId));

  useEffect(() => {
    listVideoModels().then((m) => {
      const ordered = ["grok-imagine-video", ...m.filter((x) => x !== "grok-imagine-video")];
      setModels(ordered);
    }).catch(() => {});
    void vid.initMusic();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const previewItem = videos[0];
  const preview = previewItem?.asset;
  const pickAudio = async () => {
    const p = await openDialog({
      multiple: false,
      filters: [{ name: "音频", extensions: ["mp3", "wav", "m4a", "aac"] }],
    });
    if (typeof p === "string") {
      vid.saveMusic(p); // persist so it's remembered next time
      vid.set({ musicEnabled: true });
    }
  };

  const run = async () => {
    if (!vid.prompt.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      const sourceMaterials = [
        ...(textSource ? [snapshotAsset(textSource, getDocumentMeta(textSource)?.text, "剧本/分镜原始资料")] : []),
        ...(guideAsset ? [snapshotAsset(guideAsset, undefined, "画面参考资料")] : []),
      ];
      await generateVideo(vid, {
        originalInput: vid.prompt,
        generationInput: vid.prompt,
        sourceMaterials,
        parentAssetIds: [textSource?.asset.id, guideAsset?.asset.id].filter((id): id is string => !!id),
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const modelOptions = vid.model && !models.includes(vid.model) ? [vid.model, ...models] : models;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-2 border-b border-slate-800 bg-slate-900/40 px-6 py-2.5">
        <span className="text-xs font-medium text-slate-400">视频模型</span>
        <select
          value={vid.model}
          onChange={(e) => vid.set({ model: e.target.value })}
          className="rounded-lg border border-slate-600 bg-slate-900/70 px-3 py-1.5 text-xs text-slate-100 outline-none transition focus:border-fuchsia-400"
        >
          {modelOptions.length
            ? modelOptions.map((m) => (<option key={m} value={m}>{m}</option>))
            : <option value={vid.model}>{vid.model}</option>}
        </select>
        <span className="ml-auto text-[11px] text-slate-500">单次 ≤15s，更长自动分段拼接</span>
      </div>
      <WorkflowGuide current="导入剧本/分镜并整理视频提示词" next="确认画幅和时长后生成；历史视频可加载参数继续修改" detail="本地图片导入画面描述，直接图生视频仍需公网 URL" />

      <div className="flex min-h-0 flex-1 gap-6 overflow-y-auto p-6">
        {/* Left form */}
        <div className="w-[340px] shrink-0 space-y-4 rounded-2xl border border-slate-800 bg-slate-900/60 p-5">
          <div className="flex items-center gap-2 text-sm font-semibold">
            <span className="flex h-7 w-7 items-center justify-center rounded-lg bg-fuchsia-500">
              <Clapperboard size={15} className="text-white" />
            </span>
            文生视频 / 图生视频
          </div>

          <Field label="提示词">
            <textarea className={`${inputCls} h-24 resize-y leading-snug`} value={vid.prompt} placeholder="描述视频内容…" onChange={(e) => { vid.set({ prompt: e.target.value }); setTextSource(null); }} />
            <button type="button" onClick={() => setPickerOpen(true)} className="mt-1 flex items-center gap-1 text-[10px] text-cyan-200/75 hover:text-white"><FileInput size={11} /> 从项目历史导入剧本、分镜或图片</button>
            {guideAsset && (
              <div className="mt-2 flex items-center gap-2 rounded-xl border border-cyan-300/10 bg-cyan-300/[0.035] p-2">
                <button onClick={() => setPlaying(guideAsset)} title="查看完整图片资源" className="shrink-0"><img src={convertFileSrc(guideAsset.asset.path)} alt={guideAsset.source} className="h-12 w-12 rounded-lg object-cover" /></button>
                <div className="min-w-0 flex-1"><div className="truncate text-[10px] text-cyan-100">创作参考：{guideAsset.source}</div><div className="mt-0.5 text-[9px] text-slate-600">已导入该图片的生成提示词；本地图片不会冒充公网 URL 上传。</div></div>
                <button onClick={() => setGuideAsset(null)} className="rounded p-1 text-slate-500 hover:text-rose-300"><X size={12} /></button>
              </div>
            )}
          </Field>

          <Field label="参考图公网 URL（可选）" hint="留空=文生视频">
            <input
              type="url"
              className={inputCls}
              value={vid.referencePath}
              placeholder="https://example.com/reference.png"
              onChange={(e) => vid.set({ referencePath: e.target.value })}
              spellCheck={false}
            />
            <p className="mt-1 text-[10px] leading-snug text-slate-500">视频网关需能从公网访问该图片，本地文件路径不可用。</p>
            {/^https?:\/\//i.test(vid.referencePath.trim()) && (
              <img src={vid.referencePath.trim()} alt="参考图预览" className="mt-2 h-20 w-full rounded-lg border border-slate-700 object-contain" />
            )}
          </Field>

          <Field label="时长（秒）" hint={`最多 ${vid.duration_s}s`}>
            <select className={inputCls} value={String(vid.duration_s)} onChange={(e) => vid.set({ duration_s: Number(e.target.value) })}>
              {DURATIONS.map((d) => (<option key={d} value={d}>{d} 秒</option>))}
            </select>
          </Field>

          <div className="grid grid-cols-2 gap-3">
            <Field label="画幅">
              <select className={inputCls} value={vid.aspectRatio} onChange={(e) => vid.set({ aspectRatio: e.target.value })}>
                {ASPECT_RATIOS.map((ratio) => <option key={ratio}>{ratio}</option>)}
              </select>
            </Field>
            <Field label="分辨率">
              <select className={inputCls} value={vid.resolution} onChange={(e) => vid.set({ resolution: e.target.value })}>
                {["480p", "720p", "1080p"].map((resolution) => <option key={resolution}>{resolution}</option>)}
              </select>
            </Field>
          </div>

          <Field label="背景音乐" hint={vid.musicEnabled ? "生成后自动混入" : "静音"}>
            <div className="flex gap-2">
              <div className="flex flex-1 gap-1 rounded-lg border border-slate-600 bg-slate-900/70 p-1">
                <button
                  type="button"
                  onClick={() => vid.set({ musicEnabled: false })}
                  className={`flex-1 rounded-md px-2 py-1 text-xs ${!vid.musicEnabled ? "bg-slate-700 text-white" : "text-slate-400 hover:text-white"}`}
                >
                  无音乐
                </button>
                <button
                  type="button"
                  onClick={() => { vid.set({ musicEnabled: true }); if (!vid.musicPath) void pickAudio().catch((error) => logEvent("warn", "audio.pick_failed", { error: String(error) })); }}
                  className={`flex-1 rounded-md px-2 py-1 text-xs ${vid.musicEnabled ? "bg-fuchsia-500 text-white" : "text-slate-400 hover:text-white"}`}
                >
                  导入音乐
                </button>
              </div>
              {vid.musicEnabled && vid.musicPath && (
                <button onClick={() => void pickAudio().catch((error) => logEvent("warn", "audio.pick_failed", { error: String(error) }))} className="shrink-0 rounded-lg border border-slate-600 px-2 py-1.5 text-[10px] text-slate-300 hover:bg-slate-800">更换</button>
              )}
            </div>
            {vid.musicEnabled && (
              <div className="mt-2 flex items-center gap-2 rounded-lg border border-slate-700 bg-slate-800/50 px-2 py-1.5">
                <Music size={13} className="shrink-0 text-slate-400" />
                {vid.musicPath ? (
                  <span className="min-w-0 truncate text-[10px] text-slate-300">{vid.musicPath.split(/[\\/]/).pop()}</span>
                ) : (
                  <span className="text-[10px] text-slate-500">请选择音乐文件</span>
                )}
                {vid.musicPath && (
                  <button onClick={() => vid.set({ musicEnabled: false })} className="ml-auto text-slate-500 hover:text-rose-300"><X size={13} /></button>
                )}
              </div>
            )}
          </Field>

          {error && <div className="rounded-lg bg-rose-500/10 px-3 py-2 text-xs text-rose-300">{error}</div>}

          <button onClick={run} disabled={!vid.prompt.trim() || busy} className="flex w-full items-center justify-center gap-2 rounded-lg bg-fuchsia-500 px-3 py-2.5 text-sm font-medium text-white transition hover:bg-fuchsia-400 disabled:cursor-not-allowed disabled:opacity-50">
            {busy ? <Loader2 size={15} className="animate-spin" /> : <Send size={15} />}
            {busy ? "生成中…" : "生成视频"}
          </button>
          <p className="text-[10px] leading-snug text-slate-500">超过 15 秒自动分段，本地无损拼接；音乐由内置合成器混入。</p>
        </div>

        {/* Right preview + history */}
        <div className="min-w-0 flex-1">
          <div className="flex h-full flex-col gap-4">
            <div className="rounded-2xl border border-slate-800 bg-slate-900/40 p-5">
              <div className="mb-2 flex items-center justify-between text-xs font-semibold uppercase tracking-wide text-slate-400">
                <span>视频预览</span>
              </div>
              <div className="flex min-h-[320px] items-center justify-center rounded-xl border border-dashed border-slate-700 bg-slate-800/30">
                {preview ? (
                  <div className="relative w-full"><video src={convertFileSrc(preview.path)} controls className="max-h-[60vh] w-full rounded-lg object-contain" /><button onClick={() => previewItem && setPlaying(previewItem)} className="absolute right-2 top-2 rounded-lg border border-white/10 bg-slate-950/75 px-2 py-1 text-[10px] text-cyan-200">完整详情</button></div>
                ) : busy ? (
                  <Loader2 size={24} className="animate-spin text-slate-500" />
                ) : (
                  <div className="text-center text-sm text-slate-500">输入提示词，点「生成视频」</div>
                )}
              </div>
            </div>

            {videos.length > 0 && (
              <div className="rounded-2xl border border-slate-800 bg-slate-900/40 p-5">
                <div className="mb-2 flex items-center justify-between">
                  <span className="text-xs font-semibold uppercase tracking-wide text-slate-400">历史视频</span>
                  <span className="text-[10px] text-slate-600">点击填充参数 · 右键菜单</span>
                </div>
                <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4">
                  {videos.map((a) => (
                    <div key={a.asset.id} className="flex aspect-video items-center justify-center overflow-hidden rounded-lg border border-slate-700 bg-slate-800/60">
                      <video
                        src={convertFileSrc(a.asset.path)}
                        muted
                        title={`${a.source}${a.model ? ` · ${a.model}` : ""}`}
                        onClick={() => setPlaying(a)}
                        onContextMenu={(e) => { e.preventDefault(); setMenu({ x: e.clientX, y: e.clientY, asset: a }); }}
                        className="max-h-full max-w-full cursor-pointer object-contain"
                      />
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>
        </div>
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={[
            { label: "播放视频", icon: menuIcons.preview, onClick: () => setPlaying(menu.asset) },
            { label: "复制到粘贴板", icon: menuIcons.copy, onClick: () => void navigator.clipboard?.writeText(menu.asset.asset.path).catch((error) => logEvent("warn", "clipboard.write_failed", { error: String(error) })) },
            { label: "打开所在文件夹", icon: menuIcons.open, onClick: () => void revealItemInDir(menu.asset.asset.path).catch((error) => logEvent("warn", "asset.reveal_failed", { error: String(error) })) },
          ]}
        />
      )}
      <AssetDetailModal asset={playing} onClose={() => setPlaying(null)} onLoadAsset={playing?.asset.kind === "video" ? (asset) => { vid.load((asset.params ?? {}) as Partial<VideoParams>); setTextSource(asset); } : undefined} />

      <AssetPicker
        open={pickerOpen}
        onClose={() => setPickerOpen(false)}
        kinds={["text", "image"]}
        title="导入剧本、分镜或图片资源"
        onPick={(a) => {
          if (a.asset.kind === "text") {
            const text = getDocumentMeta(a)?.text || String(a.params?.text ?? a.source);
            vid.set({ prompt: text });
            setTextSource(a);
          } else if (a.asset.kind === "image") {
            const sourcePrompt = typeof a.params?.prompt === "string"
              ? a.params.prompt
              : typeof (a.params?.params as Record<string, unknown> | undefined)?.prompt === "string"
                ? String((a.params?.params as Record<string, unknown>).prompt)
                : a.source;
            vid.set({ prompt: [vid.prompt.trim(), `参考画面：${sourcePrompt}`].filter(Boolean).join("\n\n") });
            setGuideAsset(a);
          }
        }}
      />
    </div>
  );
}
