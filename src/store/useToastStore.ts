import { create } from "zustand";

/**
 * Feedback for commands that run outside any dialog, such as the history
 * context menu: a progress toast is replaced by its result. Successes and
 * notices fade on their own; errors stay until dismissed.
 */
export type ToastTone = "progress" | "success" | "info" | "error";

export interface Toast {
  id: number;
  tone: ToastTone;
  text: string;
}

const LINGER_MS: Record<ToastTone, number | null> = { progress: null, success: 6000, info: 8000, error: null };
const MAX_TOASTS = 4;
const timers = new Map<number, ReturnType<typeof setTimeout>>();
let nextId = 1;

interface ToastState {
  toasts: Toast[];
  /** Shows a toast, or replaces toast `id` (a progress toast becoming its result). */
  show: (tone: ToastTone, text: string, id?: number) => number;
  dismiss: (id: number) => void;
}

export const useToastStore = create<ToastState>((set, get) => ({
  toasts: [],
  show: (tone, text, id = nextId++) => {
    clearTimeout(timers.get(id));
    timers.delete(id);
    set((state) => ({ toasts: [...state.toasts.filter((item) => item.id !== id), { id, tone, text }].slice(-MAX_TOASTS) }));
    const linger = LINGER_MS[tone];
    if (linger !== null) timers.set(id, setTimeout(() => get().dismiss(id), linger));
    return id;
  },
  dismiss: (id) => {
    clearTimeout(timers.get(id));
    timers.delete(id);
    set((state) => ({ toasts: state.toasts.filter((item) => item.id !== id) }));
  },
}));

export const toast = (tone: ToastTone, text: string, id?: number): number => useToastStore.getState().show(tone, text, id);
