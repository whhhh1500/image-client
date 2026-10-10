import { useGenerationStore, type GenerationForm } from "./useGenerationStore";
import { useVideoStore, type VideoParams } from "./useVideoStore";

/**
 * Unsaved image/video forms of the projects the user has left this session.
 *
 * Switching projects has to clear the forms (references and shots point at the
 * previous project's assets), but clearing alone threw away a half-written
 * prompt and every video shot on a round trip A→B→A. The outgoing project's
 * forms are kept here and put back when it becomes active again. Nothing is
 * persisted: a restart starts from the project profile as before.
 */
export interface ProjectDraft {
  generation: GenerationForm;
  video: VideoParams;
}

const drafts = new Map<string, ProjectDraft>();
/** The project whose drafts the generation/video stores currently hold. */
let formProjectId: string | null = null;

/**
 * The whole form, including the model/size fields the profile seeds: a
 * storyboard handoff records its approved video model and ratio, and restoring
 * the shots under the profile's model would flag the manifest as changed.
 */
function snapshot(): ProjectDraft {
  const g = useGenerationStore.getState();
  const v = useVideoStore.getState();
  return {
    generation: {
      prompt: g.prompt,
      referencePath: g.referencePath,
      references: g.references,
      importedSources: g.importedSources,
      size: g.size,
      quality: g.quality,
      background: g.background,
      model: g.model,
      count: g.count,
      promptTemplate: g.promptTemplate,
    },
    video: {
      shots: v.shots,
      model: v.model,
      aspectRatio: v.aspectRatio,
      resolution: v.resolution,
      mode: v.mode,
      images: v.images,
      videos: v.videos,
      audios: v.audios,
      storyboardSourceAssetId: v.storyboardSourceAssetId,
      productionManifest: v.productionManifest,
      importedSources: v.importedSources,
    },
  };
}

/** The project the forms belong to, or null before startup has bound one. */
export function formsProjectId(): string | null {
  return formProjectId;
}

/** Records which project the current forms belong to without touching them. */
export function bindFormsToProject(projectId: string | null) {
  formProjectId = projectId;
}

/** Keeps the current forms for the project they belong to. */
export function stashProjectDrafts() {
  if (formProjectId) drafts.set(formProjectId, snapshot());
}

/** Forgets a deleted project's forms. */
export function dropProjectDraft(projectId: string) {
  drafts.delete(projectId);
}

export function hasProjectDraft(projectId: string): boolean {
  return drafts.has(projectId);
}

/**
 * Binds the forms to `projectId` and puts back its stashed drafts, if any.
 * Call it after the profile and the cleared defaults are applied, so a project
 * without a stash keeps those defaults. Returns whether a draft was restored.
 */
export function restoreProjectDrafts(projectId: string | null): boolean {
  formProjectId = projectId;
  const draft = projectId ? drafts.get(projectId) : undefined;
  // The template link went out with the previous project's prompt; the cleared
  // defaults do not reset it, so a project without a stash must drop it here.
  if (!projectId || !draft) {
    useGenerationStore.getState().set({ promptTemplate: null });
    return false;
  }
  // The live forms are authoritative from here on; the next switch stashes them again.
  drafts.delete(projectId);
  useGenerationStore.getState().set(draft.generation);
  useVideoStore.getState().set(draft.video);
  return true;
}

/** Test helper: forget every stash and the bound project. */
export function resetProjectDrafts() {
  drafts.clear();
  formProjectId = null;
}
