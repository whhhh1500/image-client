import { create } from "zustand";

export type VideoRunPhase = "idle" | "submitting" | "processing" | "completed" | "failed";

/**
 * In-flight state of paid generation requests. It lives outside the panels
 * because switching tabs unmounts them while the request keeps running: kept
 * in component state, a remounted panel re-enabled its button and a second
 * click paid for the same work twice.
 */
interface RunState {
  imageBusy: boolean;
  imageError: string | null;
  /** Assets of the latest image run, so the preview shows the whole batch. */
  imageBatchIds: string[];
  videoBusy: boolean;
  videoError: string | null;
  videoPhase: VideoRunPhase;
  /** History task currently being retried from the asset center. */
  retryingTaskId: string | null;
  patch: (patch: Partial<Omit<RunState, "patch" | "clearFinished">>) => void;
  /** Drops finished-run results on project switch; in-flight flags stay. */
  clearFinished: () => void;
}

export const useRunStore = create<RunState>((set) => ({
  imageBusy: false,
  imageError: null,
  imageBatchIds: [],
  videoBusy: false,
  videoError: null,
  videoPhase: "idle",
  retryingTaskId: null,
  patch: (patch) => set(patch),
  clearFinished: () => set((state) => ({
    imageError: null,
    imageBatchIds: [],
    videoError: null,
    videoPhase: state.videoBusy ? state.videoPhase : "idle",
  })),
}));
