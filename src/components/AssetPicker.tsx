import { useMemo, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Eye, FileText, Image as ImageIcon, Search, Video, X } from "lucide-react";
import { useLibraryStore, type LibAsset } from "../store/useLibraryStore";
import { useProjectStore } from "../store/useProjectStore";
import AssetDetailModal from "./AssetDetailModal";
import { getDocumentDisplayVersion, getDocumentMeta } from "../lib/documents";
import { readTextAsset } from "../lib/ipc";

type PickerKind = "text" | "image" | "video";

export function assetMatchesPickerProject(assetProjectId: string | null | undefined, activeProjectId: string | null, strictProject: boolean): boolean {
  return strictProject ? assetProjectId === activeProjectId : assetProjectId === activeProjectId || !assetProjectId;
}

export function isAssetPickerSelectionCurrent(
  assetProjectId: string | null | undefined,
  selectedProjectId: string | null,
  currentProjectId: string | null,
  strictProject: boolean,
): boolean {
  return selectedProjectId === currentProjectId && assetMatchesPickerProject(assetProjectId, currentProjectId, strictProject);
}

/** Synchronous companion to picker state; prevents a second click before React re-renders. */
export function reserveAssetPickerPick(inFlight: { current: string | null }, assetId: string): boolean {
  if (inFlight.current) return false;
  inFlight.current = assetId;
  return true;
}

export function releaseAssetPickerPick(inFlight: { current: string | null }): void {
  inFlight.current = null;
}

export default function AssetPicker({
  open,
  onClose,
  onPick,
  kinds,
  title = "从项目资源导入",
  strictProject = false,
  filter,
}: {
  open: boolean;
  onClose: () => void;
  onPick: (asset: LibAsset) => void | Promise<void>;
  kinds: PickerKind[];
  title?: string;
  /** When enabled, hide unscoped and foreign-project assets instead of the legacy fallback. */
  strictProject?: boolean;
  /** Optional domain filter applied after project and asset-kind isolation. */
  filter?: (asset: LibAsset) => boolean;
}) {
  const allAssets = useLibraryStore((state) => state.assets);
  const projectId = useProjectStore((state) => state.activeId);
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<PickerKind>(kinds[0]);
  const [preview, setPreview] = useState<LibAsset | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pickingAssetId, setPickingAssetId] = useState<string | null>(null);
  // State updates are not synchronous enough to protect two rapid click events.
  // Keep the lock in a ref and use state only to render the pending affordance.
  const pickingAssetRef = useRef<string | null>(null);

  const availableKinds = kinds.filter((value, index) => kinds.indexOf(value) === index);
  const activeKind = availableKinds.includes(kind) ? kind : availableKinds[0];
  const assets = useMemo(() => {
    const term = query.trim().toLowerCase();
    return allAssets
      .filter((asset) => assetMatchesPickerProject(asset.projectId, projectId, strictProject) && asset.asset.kind === activeKind)
      .filter((asset) => !filter || filter(asset))
      .filter((asset) => {
        if (!term) return true;
        const document = getDocumentMeta(asset);
        const prompt = typeof asset.params?.prompt === "string" ? asset.params.prompt : "";
        return [asset.source, asset.model, document?.title, document?.text, prompt]
          .filter(Boolean)
          .some((value) => String(value).toLowerCase().includes(term));
      })
      .sort((a, b) => b.createdAt - a.createdAt);
  }, [activeKind, allAssets, filter, projectId, query, strictProject]);

  if (!open) return null;

  const pick = async (asset: LibAsset) => {
    if (!reserveAssetPickerPick(pickingAssetRef, asset.asset.id)) return;
    const selectedProjectId = projectId;
    setPickingAssetId(asset.asset.id);
    setError(null);
    try {
      let selected = asset;
      if (asset.asset.kind === "text" && !getDocumentMeta(asset)?.text) {
        const text = await readTextAsset(asset.asset.path);
        selected = { ...asset, params: { ...(asset.params ?? {}), text } };
      }
      if (!isAssetPickerSelectionCurrent(asset.projectId, selectedProjectId, useProjectStore.getState().activeId, strictProject)) {
        throw new Error("项目已切换，未导入旧项目资源；请重新选择。");
      }
      await onPick(selected);
      onClose();
    } catch (cause) {
      setError(String(cause));
    } finally {
      releaseAssetPickerPick(pickingAssetRef);
      setPickingAssetId(null);
    }
  };

  const isPicking = Boolean(pickingAssetId);
  const requestClose = () => {
    if (!isPicking) onClose();
  };

  return (
    <>
      <div className="fixed inset-0 z-50 flex items-center justify-center bg-[#020617]/80 p-4 backdrop-blur-md" onClick={requestClose}>
        <section className="dream-dialog flex max-h-[88vh] w-full max-w-3xl flex-col overflow-hidden rounded-2xl border border-cyan-200/15 bg-slate-950/92" onClick={(event) => event.stopPropagation()}>
          <header className="flex items-center gap-3 border-b border-white/5 px-5 py-4">
            <div className="min-w-0 flex-1">
              <div className="text-sm font-semibold text-slate-100">{title}</div>
              <div className="mt-0.5 text-[10px] text-slate-600">{strictProject ? "仅显示当前项目资源；先查看完整内容，再导入到当前步骤。" : "搜索全部历史；先查看完整内容，再导入到当前步骤。"}</div>
            </div>
            <button disabled={isPicking} onClick={requestClose} className="rounded-lg p-1 text-slate-500 hover:bg-white/5 hover:text-white disabled:opacity-40"><X size={16} /></button>
          </header>

          <div className="border-b border-white/5 px-5 py-3">
            <div className="flex flex-wrap items-center gap-2">
              {availableKinds.map((value) => (
                <button
                  key={value}
                  onClick={() => setKind(value)}
                  className={`rounded-lg border px-3 py-1.5 text-xs transition ${activeKind === value ? "border-cyan-300/25 bg-cyan-300/10 text-cyan-100" : "border-white/5 text-slate-500 hover:border-white/15 hover:text-slate-300"}`}
                >
                  {value === "text" ? "文档" : value === "image" ? "图片" : "视频"}
                </button>
              ))}
              <label className="relative ml-auto min-w-52 flex-1 sm:max-w-xs">
                <Search size={13} className="absolute left-3 top-1/2 -translate-y-1/2 text-slate-600" />
                <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索标题、内容、模型或提示词" className="w-full rounded-lg border border-white/8 bg-slate-950/65 py-2 pl-8 pr-3 text-xs text-slate-200 outline-none focus:border-cyan-300/30" />
              </label>
            </div>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto p-5">
            {error && <div className="mb-3 rounded-lg border border-rose-300/15 bg-rose-300/8 px-3 py-2 text-xs text-rose-200">导入失败：{error}</div>}
            {!assets.length && (
              <div className="flex min-h-52 items-center justify-center rounded-xl border border-dashed border-white/8 text-center text-xs text-slate-600">
                没有匹配的{activeKind === "text" ? "文档" : activeKind === "image" ? "图片" : "视频"}。<br />先在对应工作区生成，或换一个搜索词。
              </div>
            )}

            {activeKind === "text" && (
              <div className="space-y-2">
                {assets.map((asset) => {
                  const meta = getDocumentMeta(asset);
                  return (
                    <div key={asset.asset.id} className="flex items-center gap-3 rounded-xl border border-white/6 bg-white/[0.02] p-3 hover:border-cyan-300/15">
                      <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-cyan-300/8 text-cyan-200"><FileText size={15} /></span>
                      <div className="min-w-0 flex-1">
                        <div className="truncate text-xs font-medium text-slate-200">{meta?.title ?? asset.source} · v{getDocumentDisplayVersion(asset, allAssets)}</div>
                        <div className="mt-1 line-clamp-2 text-[10px] leading-relaxed text-slate-600">{meta?.text || "旧文档，打开详情后从文件读取"}</div>
                      </div>
                      <button onClick={() => setPreview(asset)} className="rounded-lg border border-white/8 p-2 text-slate-500 hover:text-cyan-200" title="完整查看"><Eye size={13} /></button>
                       <button disabled={isPicking} onClick={() => void pick(asset)} className="rounded-lg border border-cyan-300/15 bg-cyan-300/8 px-3 py-1.5 text-[10px] text-cyan-100 disabled:opacity-40">{pickingAssetId === asset.asset.id ? "导入中…" : "导入"}</button>
                    </div>
                  );
                })}
              </div>
            )}

            {activeKind === "image" && (
              <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(150px,1fr))]">
                {assets.map((asset) => (
                  <article key={asset.asset.id} className="group overflow-hidden rounded-xl border border-white/6 bg-white/[0.02]">
                    <button onClick={() => setPreview(asset)} className="relative block aspect-square w-full overflow-hidden bg-black/20">
                      <img src={convertFileSrc(asset.asset.path)} alt={asset.source} className="h-full w-full object-cover transition duration-300 group-hover:scale-[1.03]" />
                      <span className="absolute right-2 top-2 rounded-md bg-slate-950/75 p-1.5 text-slate-300"><Eye size={12} /></span>
                    </button>
                    <div className="p-2.5">
                      <div className="truncate text-[11px] text-slate-300">{asset.source}</div>
                       <button disabled={isPicking} onClick={() => void pick(asset)} className="mt-2 w-full rounded-lg border border-cyan-300/15 py-1.5 text-[10px] text-cyan-200 hover:bg-cyan-300/5 disabled:opacity-40"><ImageIcon size={11} className="mr-1 inline" />{pickingAssetId === asset.asset.id ? "导入中…" : "导入图片资源"}</button>
                    </div>
                  </article>
                ))}
              </div>
            )}

            {activeKind === "video" && (
              <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(220px,1fr))]">
                {assets.map((asset) => (
                  <article key={asset.asset.id} className="overflow-hidden rounded-xl border border-white/6 bg-white/[0.02]">
                    <video src={convertFileSrc(asset.asset.path)} muted className="aspect-video w-full bg-black object-cover" />
                    <div className="flex items-center gap-2 p-2.5">
                      <Video size={13} className="text-violet-200" />
                      <span className="min-w-0 flex-1 truncate text-[11px] text-slate-300">{asset.source}</span>
                      <button onClick={() => setPreview(asset)} className="rounded p-1 text-slate-500 hover:text-cyan-200"><Eye size={12} /></button>
                       <button disabled={isPicking} onClick={() => void pick(asset)} className="rounded border border-cyan-300/15 px-2 py-1 text-[10px] text-cyan-200 disabled:opacity-40">{pickingAssetId === asset.asset.id ? "导入中…" : "导入"}</button>
                    </div>
                  </article>
                ))}
              </div>
            )}
          </div>
        </section>
      </div>

      <AssetDetailModal asset={preview} onClose={() => setPreview(null)} onUseText={(text, selected) => void pick({ ...selected, params: { ...(selected.params ?? {}), text } })} />
    </>
  );
}
