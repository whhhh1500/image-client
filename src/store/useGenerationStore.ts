import { create } from "zustand";
import type { AssetImportRecord } from "../lib/assetImport";

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
  /** Explicit, ordered library inputs retained with the next generated asset. */
  importedSources?: AssetImportRecord[];
  size: string;
  quality: string;
  background: string;
  model: string;
  /** Images requested in one request (1–4, see MAX_IMAGE_COUNT). */
  count: number;
}

const DEFAULTS: GenParams = {
  prompt: "",
  referencePath: "",
  references: [],
  importedSources: [],
  size: "1024x1024 (1:1)",
  quality: "high",
  background: "auto",
  model: "gpt-image-2",
  count: 1,
};

interface GenerationState extends GenParams {
  set: (p: Partial<GenParams>) => void;
  load: (p: Partial<GenParams>) => void;
  reset: () => void;
}

export const useGenerationStore = create<GenerationState>((set) => ({
  ...DEFAULTS,
  set: (p) => set(p),
  // Assets saved before multi-reference imports have no fields for them.  A
  // history load must clear the current draft rather than inherit its inputs.
  load: (p) => set((s) => ({
    ...s,
    ...p,
    referencePath: p.referencePath ?? "",
    references: p.references ?? [],
    importedSources: p.importedSources ?? [],
  })),
  reset: () => set(DEFAULTS),
}));
