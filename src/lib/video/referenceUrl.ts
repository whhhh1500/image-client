/** Provider references must be directly reachable from the public internet. */
export function isPublicHttpsUrl(value: string): boolean {
  try {
    const url = new URL(value);
    if (url.protocol !== "https:" || url.username || url.password) return false;
    const host = url.hostname.toLowerCase().replace(/^\[|\]$/g, "");
    if (!host || host === "localhost" || [".localhost", ".local", ".internal", ".lan"].some((suffix) => host.endsWith(suffix))) return false;
    const ipv4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/)?.slice(1).map(Number);
    if (ipv4) {
      if (ipv4.some((part) => part > 255)) return false;
      const [a, b] = ipv4;
      return !(a === 0 || a === 10 || a === 127 || a >= 224 || (a === 100 && b >= 64 && b <= 127) || (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168));
    }
    if (host.includes(":")) {
      return !(host === "::" || host === "::1" || /^(?:fc|fd|fe[89ab]|ff)/i.test(host));
    }
    return host.includes(".");
  } catch {
    return false;
  }
}
