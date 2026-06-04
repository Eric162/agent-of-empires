import { createTwoFilesPatch } from "diff";

/** Unified diff for test fixtures, standing in for the server-computed
 *  `patch` field of the contents endpoint. */
export function makePatch(
  path: string,
  oldContent: string,
  newContent: string,
): string {
  return createTwoFilesPatch(`a/${path}`, `b/${path}`, oldContent, newContent);
}

/**
 * Unified diff for two files that share no lines (one hunk: every old line
 * deleted, every new line added). Avoids running a real diff algorithm in the
 * test process for huge all-different fixtures.
 */
export function makeAllDifferentPatch(
  path: string,
  oldContent: string,
  newContent: string,
): string {
  const oldLines = oldContent.split("\n").filter((_, i, a) => i < a.length - 1 || a[i] !== "");
  const newLines = newContent.split("\n").filter((_, i, a) => i < a.length - 1 || a[i] !== "");
  return (
    `--- a/${path}\n+++ b/${path}\n` +
    `@@ -1,${oldLines.length} +1,${newLines.length} @@\n` +
    oldLines.map((l) => `-${l}`).join("\n") +
    "\n" +
    newLines.map((l) => `+${l}`).join("\n") +
    "\n"
  );
}
