import { describe, expect, it } from "vitest";
import { emptyPromptLibrarySelection, promptLibraryBoundaryView, promptLibraryMountPolicy } from "./GeneratePanel";

describe("PromptLibrary deferred mount policy", () => {
  it("does not mount before first open", () => {
    expect(promptLibraryMountPolicy(false, false)).toEqual({ mounted: false, visible: false });
  });

  it("mounts and shows on first open", () => {
    expect(promptLibraryMountPolicy(true, true)).toEqual({ mounted: true, visible: true });
  });

  it("stays mounted but hidden after close so internal state can survive", () => {
    expect(promptLibraryMountPolicy(true, false)).toEqual({ mounted: true, visible: false });
  });

  it("never renders rejected lazy children after dismissing the error", () => {
    expect(promptLibraryBoundaryView(true, true)).toBe("error");
    expect(promptLibraryBoundaryView(true, false)).toBe("hidden");
    expect(promptLibraryBoundaryView(false, true)).toBe("children");
  });

  it("clears the template id and cached entry together", () => {
    expect(emptyPromptLibrarySelection<{ title: string }>()).toEqual({ id: null, entry: null });
  });
});
