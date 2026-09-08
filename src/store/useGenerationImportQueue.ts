import { create } from "zustand";
import type { ImportEntry } from "../lib/assetImport";

/**
 * App/AssetsPage bridge. It intentionally carries a request only: pages own
 * their validation and are the only place that mutates generation params.
 */
export interface PendingGenerationAssetImport {
  requestId: string;
  projectId?: string;
  entries: ImportEntry[];
  target: "image" | "video";
  action: "prompt" | "merge_prompt" | "reference";
}

interface GenerationImportQueueState {
  pending?: PendingGenerationAssetImport;
  queue: (input: Omit<PendingGenerationAssetImport, "requestId">) => void;
  clear: (requestId: string) => void;
  discard: () => void;
}

export const useGenerationImportQueue = create<GenerationImportQueueState>((set) => ({
  pending: undefined,
  queue: (input) => set({ pending: { ...input, requestId: typeof crypto?.randomUUID === "function" ? crypto.randomUUID() : `import-${Date.now()}` } }),
  clear: (requestId) => set((state) => state.pending?.requestId === requestId ? { pending: undefined } : state),
  discard: () => set({ pending: undefined }),
}));

export function queueGenerationAssetImport(input: Omit<PendingGenerationAssetImport, "requestId">) {
  useGenerationImportQueue.getState().queue(input);
}
