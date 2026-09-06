import { useCallback, useEffect, useRef, useState } from "react";
import { comicMdWorkspaceGet, type MdScope, type MdWorkspace } from "../lib/comic/markdownApi";

/** One mounted chapter owns its requests. Late reads cannot replace another chapter. */
export function useComicMdWorkspace(scope: MdScope) {
  const [workspace, setWorkspace] = useState<MdWorkspace | null>(null);
  const [error, setError] = useState("");
  const request = useRef(0);
  const active = useRef(true);
  const refresh = useCallback(async () => {
    const token = ++request.current;
    try {
      const next = await comicMdWorkspaceGet(scope);
      if (active.current && token === request.current) { setWorkspace(next); setError(""); }
    } catch (cause) { if (active.current && token === request.current) setError(String(cause)); }
  }, [scope.projectId, scope.novelWorkId, scope.chapterId]);
  useEffect(() => {
    active.current = true;
    void refresh();
    const timer = window.setInterval(() => void refresh(), 3000);
    return () => { active.current = false; request.current++; window.clearInterval(timer); };
  }, [refresh]);
  return { workspace, error, refresh };
}
