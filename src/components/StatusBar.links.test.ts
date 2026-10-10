import { describe, expect, it } from "vitest";
// Read the allow-list straight from the Go backend so the check cannot drift
// from what the packaged app actually enforces.
import systemSource from "../../internal/app/system.go?raw";
import { GITHUB_REPO_URL, ZZONE_INVITE_URL } from "./StatusBar";

/**
 * Every URL the status bar opens must be on the backend's open_url
 * allow-list. Getting this wrong fails at runtime with "不允许打开该链接":
 * the button looks fine and no browser opens.
 */
function allowedUrls(): string[] {
  const constant = (name: string) => systemSource.match(new RegExp(`${name}\\s*=\\s*"([^"]+)"`))?.[1];
  return [constant("inviteURL"), constant("repoURL")].filter((url): url is string => Boolean(url));
}

describe("external links", () => {
  const allowed = allowedUrls();

  it("keeps a narrow allow-list instead of allowing every host", () => {
    expect(allowed).toHaveLength(2);
    expect(allowed.every((url) => url.startsWith("https://"))).toBe(true);
  });

  it.each([
    ["GitHub 仓库", GITHUB_REPO_URL],
    ["ZZone 邀请链接", ZZONE_INVITE_URL],
  ])("allows the %s URL", (_label, url) => {
    expect(allowed).toContain(url);
  });
});
