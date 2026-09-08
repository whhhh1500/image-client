import { useCallback, useEffect, useRef, useState } from "react";
import { comicMdWorkspaceGet, type MdScope, type MdWorkspace } from "../lib/comic/markdownApi";

/** Cheap change detector so an unchanged poll does not re-render the workspace. */
function workspaceSignature(workspace: MdWorkspace): string {
  return [
    workspace.sourceRevisionId ?? "",
    workspace.documents.map((doc) => `${doc.id}:${doc.revision}:${doc.stale ? 1 : 0}:${doc.issues.length}`).join("|"),
    workspace.jobs.map((job) => `${job.id}:${job.status}:${job.completedPages}/${job.totalPages}`).join("|"),
    workspace.images.map((image) => `${image.id}:${image.stale ? 1 : 0}:${image.fileAvailable === false ? 0 : 1}`).join("|"),
    workspace.syncPlan?.fingerprint ?? "",
    String(workspace.workVisualProfile?.revision ?? 0),
  ].join("§");
}

/** One mounted chapter owns its requests. Late reads cannot replace another chapter. */
export function useComicMdWorkspace(scope: MdScope) {
  const [workspace, setWorkspace] = useState<MdWorkspace | null>(null);
  const [error, setError] = useState("");
  const request = useRef(0);
  const active = useRef(true);
  const signature = useRef("");
  const refresh = useCallback(async (): Promise<boolean> => {
    const token = ++request.current;
    try {
      const next = await comicMdWorkspaceGet(scope);
      if (active.current && token === request.current) {
        const nextSignature = workspaceSignature(next);
        if (nextSignature !== signature.current) {
          signature.current = nextSignature;
          setWorkspace(next);
        }
        setError("");
      }
      return true;
    } catch (cause) {
      if (active.current && token === request.current) setError(String(cause));
      return false;
    }
  }, [scope.projectId, scope.novelWorkId, scope.chapterId]);
  useEffect(() => {
    active.current = true;
    void refresh();
    const timer = window.setInterval(() => {
      // Polling a hidden window only burns IPC and re-renders.
      if (typeof document !== "undefined" && document.visibilityState === "hidden") return;
      void refresh();
    }, 3000);
    return () => { active.current = false; request.current++; window.clearInterval(timer); };
  }, [refresh]);
  return { workspace, error, refresh };
}
