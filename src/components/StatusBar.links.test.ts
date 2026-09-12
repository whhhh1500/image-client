import { describe, expect, it } from "vitest";
// Read the capability straight from the Tauri config so the check cannot drift
// from what the packaged app actually ships.
import capabilityRaw from "../../src-tauri/capabilities/default.json?raw";
import { GITHUB_REPO_URL, ZZONE_INVITE_URL } from "./StatusBar";

/**
 * Every URL the status bar hands to the opener plugin must be inside the
 * capability allow-list. Getting this wrong fails silently at runtime: the
 * button looks fine, the click only writes a log line and no browser opens.
 */
interface CapabilityPermission {
  identifier: string;
  allow?: { url?: string }[];
}

function openerUrlScope(): string[] {
  const capability = JSON.parse(capabilityRaw) as { permissions: (string | CapabilityPermission)[] };
  const permission = capability.permissions.find(
    (entry): entry is CapabilityPermission =>
      typeof entry === "object" && entry.identifier === "opener:allow-open-url",
  );
  return (permission?.allow ?? []).flatMap((entry) => (entry.url ? [entry.url] : []));
}

/** Glob match as the opener scope applies it: `**` spans anything. */
function matchesScope(url: string, pattern: string): boolean {
  const escaped = pattern
    .replace(/[.+?^${}()|[\]\\]/g, "\\$&")
    .replace(/\*\*/g, "\u0000")
    .replace(/\*/g, "[^/]*")
    .replace(/\u0000/g, ".*");
  return new RegExp(`^${escaped}$`).test(url);
}

describe("external links", () => {
  const scope = openerUrlScope();

  it("keeps a narrow opener scope instead of allowing every host", () => {
    expect(scope.length).toBeGreaterThanOrEqual(2);
    expect(scope.some((pattern) => pattern === "*" || pattern === "https://**")).toBe(false);
  });

  it.each([
    ["GitHub 仓库", GITHUB_REPO_URL],
    ["ZZone 邀请链接", ZZONE_INVITE_URL],
  ])("allows the %s URL", (_label, url) => {
    expect(scope.some((pattern) => matchesScope(url, pattern))).toBe(true);
  });
});
