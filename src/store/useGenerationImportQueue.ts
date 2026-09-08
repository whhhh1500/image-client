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
  /** Requests waiting behind `pending`; promoted in FIFO order when one clears. */
  waiting: PendingGenerationAssetImport[];
  queue: (input: Omit<PendingGenerationAssetImport, "requestId">) => void;
  clear: (requestId: string) => void;
  discard: () => void;
}

function newRequestId() {
  return typeof crypto !== "undefined" && typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `import-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export const useGenerationImportQueue = create<GenerationImportQueueState>((set) => ({
  pending: undefined,
  waiting: [],
  // A second request used to overwrite the first one silently. Keep it instead
  // so both explicit Assets-page actions are honoured in order.
  queue: (input) => set((state) => {
    const request = { ...input, requestId: newRequestId() };
    if (!state.pending) return { pending: request };
    return { waiting: [...state.waiting, request] };
  }),
  clear: (requestId) => set((state) => {
    if (state.pending?.requestId !== requestId) return state;
    const [next, ...rest] = state.waiting;
    return { pending: next, waiting: rest };
  }),
  discard: () => set({ pending: undefined, waiting: [] }),
}));

export function queueGenerationAssetImport(input: Omit<PendingGenerationAssetImport, "requestId">) {
  useGenerationImportQueue.getState().queue(input);
}
