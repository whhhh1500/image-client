import { dbExecute, dbSelect } from "./db";
import { logEvent } from "./logger";
import { persistAssetsBatch, type AssetRef } from "./ipc";
import type { AssetKind } from "../types";
import { useLibraryStore, type LibAsset, type TaskRecord, type AssetMeta } from "../store/useLibraryStore";

let historyRefreshPromise: Promise<{ assets: LibAsset[]; tasks: TaskRecord[] }> | null = null;
let historyRefreshQueued = false;

/** Best-effort: insert produced assets into the `assets` table in one transaction. */
export async function persistAssets(assets: AssetRef[], source: string, meta?: AssetMeta) {
  if (!assets.length) return;
  try {
    await persistAssetsBatch(assets, source, meta?.model, meta?.projectId, meta?.params ?? {});
  } catch (e) {
    logEvent("error", "database.persist_assets.failed", { source, assetCount: assets.length, error: String(e) });
    throw e;
  }
}

/** Best-effort: insert a task-history row into the `tasks` table. */
export async function persistTask(t: {
  id: string;
  nodeId: string;
  providerId: string;
  status: string;
  model?: string;
  label?: string;
  projectId?: string;
  kind?: string;
  createdAt: number;
  finishedAt?: number;
  error?: string;
  params?: Record<string, unknown>;
}) {
  try {
    await dbExecute(
      `INSERT INTO tasks (id, node_id, provider_id, status, input_json, error, created_at, finished_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(id) DO UPDATE SET
         node_id = excluded.node_id,
         provider_id = excluded.provider_id,
         status = excluded.status,
         input_json = excluded.input_json,
         error = excluded.error,
         finished_at = excluded.finished_at`,
      [
        t.id,
        t.nodeId,
        t.providerId,
        t.status,
        JSON.stringify({ model: t.model ?? "", label: t.label ?? "任务", projectId: t.projectId ?? "", kind: t.kind ?? "", params: t.params ?? null }),
        t.error ?? null,
        t.createdAt,
        t.finishedAt ?? null,
      ],
    );
  } catch (e) {
    logEvent("error", "database.persist_task.failed", { taskId: t.id, status: t.status, error: String(e) });
    throw e;
  }
}

export async function updateAssetMetadata(asset: LibAsset, source: string, params: Record<string, unknown>) {
  const rows = await dbSelect<{ metadata: string | null }[]>(
    "SELECT metadata FROM assets WHERE id = ?",
    [asset.asset.id],
  );
  if (!rows.length) throw new Error("资源不存在，无法保存修改");
  let existing: Record<string, unknown> = {};
  try {
    const parsed = JSON.parse(rows[0].metadata ?? "{}");
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      existing = parsed as Record<string, unknown>;
    }
  } catch {
    logEvent("warn", "database.asset_metadata_parse_failed", { assetId: asset.asset.id });
  }
  // Other producers (e.g. comic visual runs) own lineage keys at the metadata
  // root. This form only owns these four keys, so merge instead of replacing.
  const metadata = JSON.stringify({
    ...existing,
    source,
    model: asset.model,
    projectId: asset.projectId,
    params,
  });
  const result = await dbExecute("UPDATE assets SET metadata = ? WHERE id = ?", [metadata, asset.asset.id]);
  if (!result.rowsAffected) throw new Error("资源不存在，无法保存修改");
}

interface AssetRow {
  id: string;
  kind: string;
  path: string;
  width: number | null;
  height: number | null;
  duration_s: number | null;
  format: string | null;
  created_at: number;
  metadata: string | null;
}

interface TaskRow {
  id: string;
  node_id: string;
  status: string;
  input_json: string | null;
  error: string | null;
  created_at: number;
  finished_at: number | null;
}

/** Load persisted assets + task history from the DB (for the 资产/任务 tabs). */
export async function loadHistory() {
  const assetRows = await dbSelect<AssetRow[]>(
    "SELECT id, kind, path, width, height, duration_s, format, created_at, metadata FROM assets ORDER BY created_at DESC",
  );
  const taskRows = await dbSelect<TaskRow[]>(
    "SELECT id, node_id, status, input_json, error, created_at, finished_at FROM tasks ORDER BY created_at DESC",
  );

  const assets: LibAsset[] = assetRows.map((r) => {
    let source = "历史";
    let model: string | undefined;
    let projectId: string | undefined;
    let params: LibAsset["params"];
    try {
      const meta = JSON.parse(r.metadata ?? "{}");
      if (meta?.source) source = meta.source;
      if (meta?.model) model = meta.model;
      if (meta?.projectId) projectId = meta.projectId;
      if (meta?.params) params = meta.params;
    } catch {
      /* ignore */
    }
    return {
      asset: {
        id: r.id,
        kind: r.kind as AssetKind,
        path: r.path,
        width: r.width ?? undefined,
        height: r.height ?? undefined,
        durationS: r.duration_s ?? undefined,
        format: r.format ?? undefined,
      },
      source,
      model,
      projectId,
      params,
      createdAt: r.created_at,
    };
  });

  const tasks: TaskRecord[] = taskRows.map((r) => {
    let model = "";
    let label = "任务";
    let projectId: string | undefined;
    let kind: TaskRecord["kind"];
    let params: Record<string, unknown> | undefined;
    try {
      const j = JSON.parse(r.input_json ?? "{}");
      model = j?.model ?? "";
      label = j?.label ?? r.node_id;
      projectId = j?.projectId ?? undefined;
      kind = j?.kind ?? "image";
      params = j?.params && typeof j.params === "object" ? j.params : undefined;
    } catch {
      /* ignore */
    }
    return {
      id: r.id,
      nodeId: r.node_id,
      label,
      model,
      projectId,
      kind: kind as TaskRecord["kind"],
      status: (r.status as TaskRecord["status"]) || "idle",
      createdAt: r.created_at,
      finishedAt: r.finished_at ?? undefined,
      error: r.error ?? undefined,
      params,
    };
  });

  return { assets, tasks };
}

/** Reload the DB-backed library without requiring an application restart. */
export async function refreshLibraryHistory(reason = "manual") {
  if (historyRefreshPromise) {
    historyRefreshQueued = true;
    return historyRefreshPromise;
  }
  historyRefreshPromise = (async () => {
    let history = { assets: [] as LibAsset[], tasks: [] as TaskRecord[] };
    do {
      historyRefreshQueued = false;
      const started = performance.now();
      history = await loadHistory();
      useLibraryStore.getState().loadAssets(history.assets);
      useLibraryStore.getState().loadTasks(history.tasks);
      logEvent("info", "history.refreshed", {
        reason,
        assetCount: history.assets.length,
        taskCount: history.tasks.length,
        durationMs: performance.now() - started,
      });
    } while (historyRefreshQueued);
    return history;
  })().catch((error) => {
    logEvent("error", "history.refresh_failed", { reason, error: String(error) });
    throw error;
  }).finally(() => {
    historyRefreshPromise = null;
  });
  return historyRefreshPromise;
}
