import { describe, expect, it } from "vitest";
import { createId } from "./id";

describe("identifier generation", () => {
  it("does not collide during a burst", () => {
    const ids = Array.from({ length: 2_000 }, () => createId("task"));
    expect(new Set(ids).size).toBe(ids.length);
    expect(ids.every((id) => id.startsWith("task_"))).toBe(true);
  });
});
