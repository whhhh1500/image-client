import { useEffect, useRef } from "react";

/** Open layers, oldest first; only the newest one answers Escape. */
const layers: symbol[] = [];

/**
 * Calls `onEscape` when Escape is pressed while this layer is the newest open
 * one, so a picker over a modal closes before the modal does. A control that
 * handles Escape itself (an open dropdown) calls preventDefault to keep the
 * layer open; Escape that ends an IME composition is ignored.
 */
export function useEscapeKey(onEscape: () => void, active = true): void {
  const handler = useRef(onEscape);
  useEffect(() => {
    handler.current = onEscape;
  });

  useEffect(() => {
    if (!active) return;
    const layer = Symbol("layer");
    layers.push(layer);
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || event.isComposing) return;
      if (layers[layers.length - 1] !== layer) return;
      event.preventDefault();
      handler.current();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      layers.splice(layers.indexOf(layer), 1);
    };
  }, [active]);
}
