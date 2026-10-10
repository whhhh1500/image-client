import { useEffect, useRef, type RefObject } from "react";

/** Open layers, oldest first; only the newest one answers Escape and Tab. */
const layers: symbol[] = [];

const FOCUSABLE = [
  "a[href]", "button:not([disabled])", "input:not([disabled]):not([type='hidden'])", "select:not([disabled])",
  "textarea:not([disabled])", "[tabindex]:not([tabindex='-1'])", "[contenteditable='true']",
].join(",");

/** Tab stops inside `root`, in document order. Visibility comes from attributes, not layout. */
function tabStops(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((node) => !node.closest("[hidden],[inert],[aria-hidden='true']"));
}

/** Wraps Tab and Shift+Tab around the ends of `root`, and pulls stray focus back in. */
function keepTabInside(event: KeyboardEvent, root: HTMLElement) {
  const stops = tabStops(root);
  const current = document.activeElement;
  if (!stops.length) {
    event.preventDefault();
    return;
  }
  const outside = !root.contains(current);
  if (event.shiftKey ? outside || current === stops[0] : outside || current === stops[stops.length - 1]) {
    event.preventDefault();
    (event.shiftKey ? stops[stops.length - 1] : stops[0]).focus();
  }
}

function useLayer(onEscape: () => void, active: boolean, container?: RefObject<HTMLElement | null>) {
  const handler = useRef(onEscape);
  useEffect(() => {
    handler.current = onEscape;
  });

  useEffect(() => {
    if (!active) return;
    const layer = Symbol("layer");
    layers.push(layer);
    const opener = container && document.activeElement instanceof HTMLElement ? document.activeElement : null;
    // After the dialog has rendered and any autoFocus inside it has run.
    const frame = container ? requestAnimationFrame(() => {
      const root = container.current;
      if (root && !root.contains(document.activeElement)) (tabStops(root)[0] ?? root).focus();
    }) : 0;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || layers[layers.length - 1] !== layer) return;
      if (event.key === "Escape" && !event.isComposing) {
        event.preventDefault();
        handler.current();
      } else if (event.key === "Tab" && container?.current) {
        keepTabInside(event, container.current);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("keydown", onKeyDown);
      layers.splice(layers.indexOf(layer), 1);
      if (opener?.isConnected) opener.focus();
    };
  }, [active, container]);
}

/**
 * Calls `onEscape` when Escape is pressed while this layer is the newest open
 * one, so a picker over a modal closes before the modal does. A control that
 * handles Escape itself (an open dropdown) calls preventDefault to keep the
 * layer open; Escape that ends an IME composition is ignored.
 */
export function useEscapeKey(onEscape: () => void, active = true): void {
  useLayer(onEscape, active);
}

/**
 * A modal dialog: Escape as in useEscapeKey, and while it is the newest layer
 * Tab cycles inside `container`. Opening moves focus into the dialog unless
 * something inside already has it (autoFocus); closing returns focus to the
 * element that had it before.
 */
export function useModalLayer(container: RefObject<HTMLElement | null>, onEscape: () => void, active = true): void {
  useLayer(onEscape, active, container);
}
