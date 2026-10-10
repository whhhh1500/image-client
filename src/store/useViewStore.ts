import { useCallback, useRef } from "react";
import { create } from "zustand";

/**
 * View state of pages that unmount when the user switches tabs: filters, the
 * open step, the page being edited. Only one tab is mounted at a time, so state
 * kept in `useState` was lost on every round trip. It lives here for the rest
 * of the session; a restart starts from each page's defaults. State that
 * belongs to a project or chapter is keyed by it so it cannot leak into another.
 */
interface ViewState {
  values: Record<string, unknown>;
  put: (key: string, value: unknown) => void;
}

export const useViewStore = create<ViewState>((set) => ({
  values: {},
  put: (key, value) => set((state) => ({ values: { ...state.values, [key]: value } })),
}));

/**
 * `useState` that survives the component unmounting; `initial` applies until
 * the key is first set. An updater reads the latest stored value, so a result
 * that arrives after an await (or after the page unmounted) still lands on top
 * of edits made meanwhile.
 */
export function useViewState<T>(key: string, initial: T) {
  const stored = useViewStore((state) => state.values[key]) as T | undefined;
  const put = useViewStore((state) => state.put);
  const initialRef = useRef(initial);
  initialRef.current = initial;
  const setValue = useCallback((next: T | ((current: T) => T)) => {
    if (typeof next !== "function") return put(key, next);
    const current = useViewStore.getState().values[key] as T | undefined;
    put(key, (next as (current: T) => T)(current === undefined ? initialRef.current : current));
  }, [key, put]);
  return [stored === undefined ? initial : stored, setValue] as const;
}

/** Test helper: forget every remembered view. */
export function resetViewState() {
  useViewStore.setState({ values: {} });
}
