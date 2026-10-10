/**
 * The message to show for a caught value. Backend failures already arrive as
 * plain strings (see desktop.ts); errors thrown in the frontend are Error
 * objects, whose String() would prefix every message with "Error: ".
 */
export function errorText(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

/**
 * Error text recorded before the backend extracted gateway reasons ends in the
 * raw JSON body (`… 500 Internal Server Error: {"error":{"message":…}}`).
 * Shows such a record the way new errors read; other text is unchanged.
 */
export function readableErrorText(text: string): string {
  const start = text.indexOf("{");
  if (start < 0) return text;
  try {
    const body = JSON.parse(text.slice(start)) as { error?: unknown; message?: unknown };
    const nested = body.error && typeof body.error === "object" ? (body.error as { message?: unknown }).message : body.error;
    const reason = [nested, body.message].find((value): value is string => typeof value === "string" && value.trim() !== "");
    if (!reason) return text;
    const prefix = text.slice(0, start).trimEnd().replace(/[:：]$/, "");
    return prefix ? `${prefix}: ${reason.trim()}` : reason.trim();
  } catch {
    return text;
  }
}
