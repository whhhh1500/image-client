import { describe, expect, it } from "vitest";
import { assetMatchesPickerProject, isAssetPickerSelectionCurrent, releaseAssetPickerPick, reserveAssetPickerPick } from "./AssetPicker";

describe("assetMatchesPickerProject", () => {
  it("keeps the legacy picker behavior by default", () => {
    expect(assetMatchesPickerProject(undefined, "project_a", false)).toBe(true);
    expect(assetMatchesPickerProject("project_b", "project_a", false)).toBe(false);
  });

  it("strictly excludes unscoped and foreign assets for comic references", () => {
    expect(assetMatchesPickerProject("project_a", "project_a", true)).toBe(true);
    expect(assetMatchesPickerProject(undefined, "project_a", true)).toBe(false);
    expect(assetMatchesPickerProject("project_b", "project_a", true)).toBe(false);
  });

  it("reserves a synchronous pending lock until the real onPick promise settles", () => {
    const pending = { current: null as string | null };
    expect(reserveAssetPickerPick(pending, "asset_a")).toBe(true);
    expect(reserveAssetPickerPick(pending, "asset_a")).toBe(false);
    expect(reserveAssetPickerPick(pending, "asset_b")).toBe(false);
    releaseAssetPickerPick(pending);
    expect(reserveAssetPickerPick(pending, "asset_b")).toBe(true);
  });

  it("rejects a selection that crosses an active-project switch", () => {
    expect(isAssetPickerSelectionCurrent("project_a", "project_a", "project_a", true)).toBe(true);
    expect(isAssetPickerSelectionCurrent("project_a", "project_a", "project_b", true)).toBe(false);
    expect(isAssetPickerSelectionCurrent(undefined, "project_a", "project_a", false)).toBe(true);
  });
});
