import { create } from "zustand";

export type ImageReferenceRole =
  | "character_identity"
  | "outfit"
  | "style"
  | "pose"
  | "scene"
  | "prop"
  | "previous_panel"
  | "base_image"
  | "mask";

export interface ImageGenerationReference {
  path: string;
  role: ImageReferenceRole;
  weight?: number;
  sortOrder?: number;
}

export interface GenParams {
  prompt: string;
  referencePath: string;
  /** Optional multi-reference contract. A non-empty list is authoritative over referencePath. */
  references?: ImageGenerationReference[];
  size: string;
  quality: string;
  background: string;
  model: string;
}

const DEFAULTS: GenParams = {
  prompt: "",
  referencePath: "",
  size: "1024x1024 (1:1)",
  quality: "high",
  background: "auto",
  model: "gpt-image-2",
};

interface GenerationState extends GenParams {
  set: (p: Partial<GenParams>) => void;
  load: (p: Partial<GenParams>) => void;
  reset: () => void;
}

export const useGenerationStore = create<GenerationState>((set) => ({
  ...DEFAULTS,
  set: (p) => set(p),
  load: (p) => set((s) => ({ ...s, ...p })),
  reset: () => set(DEFAULTS),
}));
