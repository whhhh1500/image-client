import { useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { ImagePlus, Trash2 } from "lucide-react";
import AssetPicker from "../AssetPicker";
import { comicMdWorkVisualImport, type MdScope, type MdStyleReference } from "../../lib/comic/markdownApi";
import { MAX_COMIC_STYLE_REFERENCES, createComicStyleReference, isComicStyleReferenceCandidate, normalizeComicStyleReferences } from "../../lib/comic/styleReferences";
import { useLibraryStore } from "../../store/useLibraryStore";
import { button, field } from "./mdWorkspaceState";

export default function ComicStyleReferencePanel({ scope, references, onChange, disabled }: {
  scope: MdScope;
  references: MdStyleReference[];
  onChange: (references: MdStyleReference[]) => void;
  disabled: boolean;
}) {
  const assets = useLibraryStore((state) => state.assets);
  const addAssets = useLibraryStore((state) => state.addAssets);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState("");
  const normalized = normalizeComicStyleReferences(references);
  const selected = new Set(normalized.map((item) => item.assetId));
  const selectedPaths = new Set(normalized.map((item) => item.path.toLocaleLowerCase()));
  const current = new Map(assets.map((asset) => [asset.asset.id, asset]));
  const unavailable = new Set(normalized.filter((item) => {
    const asset = current.get(item.assetId);
    return !asset || !isComicStyleReferenceCandidate(asset, scope) || asset.asset.path !== item.path;
  }).map((item) => item.assetId));
  const update = (next: MdStyleReference[]) => onChange(normalizeComicStyleReferences(next));
  const upload = async () => {
    if (uploading || normalized.length >= MAX_COMIC_STYLE_REFERENCES) return;
    setError("");
    setUploading(true);
    try {
      const picked = await openDialog({
        multiple: true,
        directory: false,
        filters: [{ name: "图片作品", extensions: ["png", "jpg", "jpeg", "webp", "gif"] }],
      });
      const paths = [...new Set((Array.isArray(picked) ? picked : typeof picked === "string" ? [picked] : []).filter((path) => !selectedPaths.has(path.toLocaleLowerCase())))].slice(0, MAX_COMIC_STYLE_REFERENCES - normalized.length);
      if (!paths.length) return;
      const imported = await comicMdWorkVisualImport({ projectId: scope.projectId, novelWorkId: scope.novelWorkId, paths });
      addAssets(imported, "漫画画风参考", { projectId: scope.projectId, params: { novelWorkId: scope.novelWorkId, comicStyleReference: true } });
      update([...normalized, ...imported.map((asset, index) => ({
        assetId: asset.id,
        path: asset.path,
        label: paths[index]?.split(/[\\/]/).pop() || `画风参考 ${normalized.length + index + 1}`,
      }))]);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setUploading(false);
    }
  };

  return <section aria-label="作品级画风参考" className="rounded-xl border border-fuchsia-300/15 bg-fuchsia-300/[0.035] p-4">
    <div className="flex flex-wrap items-start justify-between gap-3">
      <div>
        <h3 className="text-sm font-semibold text-fuchsia-100">作品级画风参考</h3>
        <p className="mt-1 max-w-3xl text-xs leading-5 text-slate-400">本小说所有章节共用这组图片；只接受当前项目的图片资源，视频资源不会出现在这里。最多 {MAX_COMIC_STYLE_REFERENCES} 张。保存后会作为真实多图参考传给漫画生图接口。</p>
      </div>
      <div className="flex flex-wrap gap-2"><button className={button} disabled={disabled || uploading || normalized.length >= MAX_COMIC_STYLE_REFERENCES} onClick={() => void upload()}><ImagePlus size={14} className="mr-1 inline" />{uploading ? "导入中…" : "上传图片作品"}</button><button className={button} disabled={disabled || uploading || normalized.length >= MAX_COMIC_STYLE_REFERENCES} onClick={() => setPickerOpen(true)}>从项目资源选择</button></div>
    </div>
    {error && <p role="alert" className="mt-3 text-xs text-rose-200">{error}</p>}
    {!normalized.length ? <p className="mt-3 rounded-lg border border-dashed border-white/10 p-3 text-xs text-slate-500">尚未选择画风图。可继续只使用作品设定中的文字画风；选择图片后请补充“画风说明”。</p> : <div className="mt-3 grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
      {normalized.map((reference, index) => <article key={reference.assetId} className="overflow-hidden rounded-lg border border-white/10 bg-slate-950/35">
        <img src={convertFileSrc(reference.path)} alt={`画风参考 ${index + 1}：${reference.label}`} className="h-32 w-full object-cover" onError={() => setError(`画风参考“${reference.label}”无法读取，请移除后重新上传。`)} />
        <div className="space-y-2 p-3">
          <div className="flex items-start gap-2"><span className="min-w-0 flex-1 break-words text-xs text-slate-200">{index + 1}. {reference.label}</span><button aria-label={`移除画风参考 ${reference.label}`} title="移除画风参考" className="rounded p-1 text-slate-500 hover:text-rose-300 disabled:opacity-40" disabled={disabled} onClick={() => update(normalized.filter((item) => item.assetId !== reference.assetId))}><Trash2 size={14} /></button></div>
          <label className="block text-[11px] text-slate-400">备注（可选）<textarea aria-label={`${reference.label} 画风说明`} disabled={disabled} value={reference.description ?? ""} onChange={(event) => update(normalized.map((item) => item.assetId === reference.assetId ? { ...item, description: event.target.value } : item))} className={`${field} mt-1 min-h-20 w-full text-xs`} placeholder="例如：主要参考线条和纸张纹理，不参考人物造型" /></label>
          {unavailable.has(reference.assetId) && <p role="alert" className="text-[11px] leading-4 text-amber-200">这张图已不在当前项目或作品作用域内。本次不会作为参考传递，请移除后重新选择。</p>}
        </div>
      </article>)}
    </div>}
    <p className="mt-3 text-[11px] leading-5 text-slate-500">参考图片只用于提取并约束画风，不把图中的人物、剧情、文字、Logo 或具体构图当作本作内容。角色身份、服装、场景和道具参考后续仍按各自资产角色单独管理。</p>
    <AssetPicker open={pickerOpen} onClose={() => setPickerOpen(false)} kinds={["image"]} strictProject filter={(asset) => isComicStyleReferenceCandidate(asset, scope) && !selected.has(asset.asset.id) && !selectedPaths.has(asset.asset.path.toLocaleLowerCase())} title="选择作品级画风参考图" onPick={(asset) => update([...normalized, createComicStyleReference(asset)])} />
  </section>;
}
