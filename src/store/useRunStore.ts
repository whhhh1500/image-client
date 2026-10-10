import { create } from "zustand";
import { appendRunLog, type ImageProgressEvent, type ImageRunLogEntry, type ImageRunLogLevel } from "../lib/imageProgress";

export type VideoRunPhase = "idle" | "submitting" | "processing" | "completed" | "failed";

/** Shot progress of the in-flight (or latest) video run. */
export interface VideoRunProgressState {
  /** Shots saved so far, including ones kept from an earlier attempt of the same group. */
  done: number;
  total: number;
  /** Shot being generated right now; null before the first and between shots. */
  currentShotNo: number | null;
  startedAt: number;
}

/** A failed video run that saved some shots, so a retry pays only for the rest. */
export interface VideoResumeState {
  shotGroupId: string;
  /** The request it was made for; resuming is only offered while the form still matches. */
  fingerprint: string;
  /**
   * The project the group belongs to. The offer outlives a project switch,
   * because the project's video draft is restored when the user comes back.
   */
  projectId: string | null;
}

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
  /** Latest backend step of the in-flight image run. */
  imageProgress: ImageProgressEvent | null;
  /** Start of the in-flight (or latest) image run; drives the elapsed timer. */
  imageRunStartedAt: number | null;
  /** Steps of the latest image run, kept after it ends until the next one. */
  imageLog: ImageRunLogEntry[];
  videoBusy: boolean;
  videoError: string | null;
  videoPhase: VideoRunPhase;
  videoProgress: VideoRunProgressState | null;
  videoResume: VideoResumeState | null;
  /** History task currently being retried from the asset center. */
  retryingTaskId: string | null;
  patch: (patch: Partial<Omit<RunState, "patch" | "clearFinished" | "logImageRun">>) => void;
  /** Append a line to the image run log, timed from the run start. */
  logImageRun: (level: ImageRunLogLevel, text: string) => void;
  /** Drops finished-run results on project switch; in-flight flags stay. */
  clearFinished: () => void;
}

export const useRunStore = create<RunState>((set) => ({
  imageBusy: false,
  imageError: null,
  imageBatchIds: [],
  imageProgress: null,
  imageRunStartedAt: null,
  imageLog: [],
  videoBusy: false,
  videoError: null,
  videoPhase: "idle",
  videoProgress: null,
  videoResume: null,
  retryingTaskId: null,
  patch: (patch) => set(patch),
  logImageRun: (level, text) => set((state) => {
    const now = Date.now();
    return { imageLog: appendRunLog(state.imageLog, { offsetMs: now - (state.imageRunStartedAt ?? now), level, text }) };
  }),
  clearFinished: () => set((state) => ({
    imageError: null,
    imageBatchIds: [],
    imageLog: state.imageBusy ? state.imageLog : [],
    videoError: null,
    videoPhase: state.videoBusy ? state.videoPhase : "idle",
    videoProgress: state.videoBusy ? state.videoProgress : null,
    // videoResume is kept: it names its project and is only offered there.
  })),
}));
