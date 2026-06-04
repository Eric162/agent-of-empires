import { parseDiffFromFile } from "@pierre/diffs";
import type { SearchableLine } from "./findMatches";

/**
 * The changed (added/deleted) lines of the diff between `oldContent` and
 * `newContent`, in rendered (top-to-bottom, deletions-before-additions per
 * change block) order.
 *
 * This is the searchable set for in-diff find: for the MVP we match only lines
 * that are actually part of the change, not unchanged context or the rest of
 * the file. Expanded-context lines (which the user can reveal) are out of
 * scope for now.
 */
export function changedLines(
  oldContent: string,
  newContent: string,
  name: string,
): SearchableLine[] {
  const meta = parseDiffFromFile(
    { name, contents: oldContent },
    { name, contents: newContent },
  );
  const out: SearchableLine[] = [];
  for (const hunk of meta.hunks) {
    for (const seg of hunk.hunkContent) {
      if (seg.type !== "change") continue;
      for (let k = 0; k < seg.deletions; k++) {
        const idx = seg.deletionLineIndex + k;
        out.push({
          side: "old",
          lineNumber: idx + 1,
          text: meta.deletionLines[idx] ?? "",
        });
      }
      for (let k = 0; k < seg.additions; k++) {
        const idx = seg.additionLineIndex + k;
        out.push({
          side: "new",
          lineNumber: idx + 1,
          text: meta.additionLines[idx] ?? "",
        });
      }
    }
  }
  return out;
}
