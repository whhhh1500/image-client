import { useCallback, useState } from "react";
import { fetchModels, type ModelCatalogKind } from "./ipc";
import { logEvent } from "./logger";

export interface ModelCatalog {
  /** Suggested models; every field that consumes this stays free-form. */
  options: string[];
  loading: boolean;
  /** Result of the last 「获取模型」 attempt, shown under the field. */
  message: string | null;
  error: boolean;
  setOptions: (options: string[]) => void;
  clearMessage: () => void;
  /**
   * Read `/v1/models` from the gateway. Credentials default to the saved
   * configuration; the settings page passes what its form currently holds.
   */
  refresh: (credentials?: { url?: string; key?: string }) => Promise<void>;
}

/**
 * Model catalog for one connection kind, shared by every place that lets the
 * user pick a model (settings defaults, image/video panels, project profile).
 */
export function useModelCatalog(kind: ModelCatalogKind, seed: string[]): ModelCatalog {
  const [state, setState] = useState<Omit<ModelCatalog, "setOptions" | "clearMessage" | "refresh">>(
    () => ({ options: seed, loading: false, message: null, error: false }),
  );

  const setOptions = useCallback((options: string[]) => {
    setState((prev) => ({ ...prev, options }));
  }, []);

  const clearMessage = useCallback(() => {
    setState((prev) => (prev.message === null && !prev.error ? prev : { ...prev, message: null, error: false }));
  }, []);

  const refresh = useCallback(
    async (credentials: { url?: string; key?: string } = {}) => {
      setState((prev) => ({ ...prev, loading: true, message: null, error: false }));
      try {
        const list = await fetchModels({
          url: credentials.url ?? "",
          key: credentials.key ?? "",
          kind,
        });
        setState({
          options: list,
          loading: false,
          message: `已获取 ${list.length} 个模型，可从下拉框选择或继续手动输入`,
          error: false,
        });
      } catch (error) {
        const reason = String(error);
        logEvent("warn", "models.fetch_failed", { kind, error: reason });
        setState((prev) => ({ ...prev, loading: false, message: `获取模型失败：${reason}`, error: true }));
      }
    },
    [kind],
  );

  return { ...state, setOptions, clearMessage, refresh };
}
