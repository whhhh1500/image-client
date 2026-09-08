import { create } from "zustand";
import type { AssetRef, NodeStatus } from "../types";

export interface LibAsset {
  asset: AssetRef;
  source: string;
  model?: string;
  projectId?: string;
  params?: Record<string, unknown>;
  createdAt: number;
}

export interface TaskRecord {
  id: string;
  nodeId: string;
  label: string;
  model?: string;
  projectId?: string;
  kind?: "image" | "video";
  status: NodeStatus;
  createdAt: number;
  finishedAt?: number;
  error?: string;
  params?: Record<string, unknown>;
}

export interface AssetMeta {
  model?: string;
  projectId?: string;
  params?: Record<string, unknown>;
  source?: string;
}

interface LibraryState {
  assets: LibAsset[];
  tasks: TaskRecord[];
  addAssets: (assets: AssetRef[], source: string, meta?: AssetMeta) => void;
  addLibraryAssets: (assets: LibAsset[]) => void;
  addTask: (t: TaskRecord) => void;
  updateTask: (id: string, patch: Partial<TaskRecord>) => void;
  updateAsset: (id: string, patch: Partial<LibAsset>) => void;
  loadAssets: (list: LibAsset[]) => void;
  loadTasks: (list: TaskRecord[]) => void;
}

export const useLibraryStore = create<LibraryState>((set) => ({
  assets: [],
  tasks: [],
  addAssets: (assets, source, meta) =>
    set((s) => ({
      assets: [
        ...assets.map((a) => ({
          asset: a,
          source,
          model: meta?.model,
          projectId: meta?.projectId,
          params: meta?.params,
          createdAt: Date.now(),
        })),
        ...s.assets,
      ],
    })),
  addLibraryAssets: (assets) => set((state) => {
    const ids = new Set(assets.map((asset) => asset.asset.id));
    return { assets: [...assets, ...state.assets.filter((asset) => !ids.has(asset.asset.id))] };
  }),
  addTask: (t) => set((s) => ({ tasks: [t, ...s.tasks] })),
  updateTask: (id, patch) =>
    set((s) => ({
      tasks: s.tasks.map((t) => (t.id === id ? { ...t, ...patch } : t)),
    })),
  updateAsset: (id, patch) =>
    set((state) => ({
      assets: state.assets.map((asset) => asset.asset.id === id ? { ...asset, ...patch } : asset),
    })),
  loadAssets: (list) => set({ assets: list }),
  loadTasks: (list) => set({ tasks: list }),
}));
