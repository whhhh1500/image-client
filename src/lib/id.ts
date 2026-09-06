let fallbackCounter = 0;

export function createId(prefix: string): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return `${prefix}_${uuid}`;
  fallbackCounter = (fallbackCounter + 1) % Number.MAX_SAFE_INTEGER;
  return `${prefix}_${Date.now()}_${fallbackCounter}_${Math.random().toString(36).slice(2, 10)}`;
}
