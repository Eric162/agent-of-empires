import { describe, it, expect } from "vitest";
import { findMatches } from "./findMatches";

const OLD = "alpha beta\ngamma\nBETA done\n";
const NEW = "alpha BETA\ndelta beta beta\n";

describe("findMatches", () => {
  it("returns nothing for an empty query", () => {
    expect(findMatches(OLD, NEW, "")).toEqual([]);
  });

  it("matches case-insensitively by default across both sides", () => {
    const m = findMatches(OLD, NEW, "beta");
    // old side first (default order), then new side.
    expect(m.map((x) => [x.side, x.lineNumber, x.startCol, x.endCol])).toEqual([
      ["old", 1, 6, 10],
      ["old", 3, 0, 4],
      ["new", 1, 6, 10],
      ["new", 2, 6, 10],
      ["new", 2, 11, 15],
    ]);
  });

  it("assigns a contiguous global index in match order", () => {
    const m = findMatches(OLD, NEW, "beta");
    expect(m.map((x) => x.index)).toEqual([0, 1, 2, 3, 4]);
  });

  it("respects caseSensitive", () => {
    const m = findMatches(OLD, NEW, "BETA", { caseSensitive: true });
    expect(m.map((x) => [x.side, x.lineNumber, x.startCol])).toEqual([
      ["old", 3, 0],
      ["new", 1, 6],
    ]);
  });

  it("finds non-overlapping literal matches", () => {
    const m = findMatches("aaaa", "", "aa", { sides: ["old"] });
    expect(m.map((x) => [x.startCol, x.endCol])).toEqual([
      [0, 2],
      [2, 4],
    ]);
  });

  it("can restrict the searched sides", () => {
    const m = findMatches(OLD, NEW, "beta", { sides: ["new"] });
    expect(m.every((x) => x.side === "new")).toBe(true);
    expect(m).toHaveLength(3);
  });

  it("supports regex search", () => {
    const m = findMatches("foo123bar", "", "\\d+", {
      regex: true,
      sides: ["old"],
    });
    expect(m.map((x) => [x.startCol, x.endCol])).toEqual([[3, 6]]);
  });

  it("does not loop forever on zero-width regex matches", () => {
    const m = findMatches("abc", "", "x*", { regex: true, sides: ["old"] });
    // One zero-width match attempt per position plus end; just assert it
    // terminates and produces finite results.
    expect(m.length).toBeGreaterThan(0);
    expect(m.every((x) => x.startCol === x.endCol)).toBe(true);
  });

  it("throws on an invalid regex so callers can show an error", () => {
    expect(() =>
      findMatches("x", "", "(", { regex: true, sides: ["old"] }),
    ).toThrow();
  });

  it("ignores the spurious trailing line from a final newline", () => {
    // OLD ends in "\n" -> 3 real lines, not 4.
    const m = findMatches("a\n", "", "a", { sides: ["old"] });
    expect(m).toEqual([
      { side: "old", lineNumber: 1, startCol: 0, endCol: 1, index: 0 },
    ]);
  });
});
