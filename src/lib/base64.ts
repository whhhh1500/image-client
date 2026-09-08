/**
 * Encode bytes for IPC transport.
 *
 * Tauri only treats the whole message as raw bytes when the top-level argument
 * is an ArrayBuffer/TypedArray. A nested `Uint8Array` is serialized as a JSON
 * array of numbers (~3.5 bytes of text per input byte, one string per element),
 * so a 100 MB video would build a ~350 MB string. Base64 costs 1.33x and no
 * per-byte allocation, and works on every IPC transport.
 */
export function encodeBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunk) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunk));
  }
  return btoa(binary);
}
