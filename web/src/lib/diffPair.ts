// Convert an `(old_string, new_string)` pair into a `RichDiffHunk`
// plus add/del counts, so the cockpit Edit/Write card can drive its
// body and its `+N −N` chip off a single line-diff pass. Uses
// `@pierre/diffs` `parseDiffFromFile`, the same diff engine the diff
// surfaces render with. See #1073 / #1074.

import { parseDiffFromFile } from "@pierre/diffs";
import type { RichDiffHunk, RichDiffLine } from "./types";

export interface DiffPairResult {
  hunk: RichDiffHunk;
  adds: number;
  dels: number;
}

/** Force a single trailing newline so the line diff doesn't treat
 *  "last line without `\n`" as a distinct token from "same line with
 *  `\n`". Without this, `"a\nb\nc"` vs `"a\nb\nc\nd"` registers as
 *  remove("c") + add("c\nd\n") instead of add("d\n"). */
function withTrailingNewline(s: string): string {
  if (s === "") return s;
  return s.endsWith("\n") ? s : s + "\n";
}

/** `parseDiffFromFile` keeps the trailing newline on every line but
 *  the file's last; drop it so `content` is the bare line text. */
function stripNewline(s: string): string {
  return s.endsWith("\n") ? s.slice(0, -1) : s;
}

/** Run a line-level diff over the pair and emit a `RichDiffHunk`
 *  shaped the same way the file-diff endpoint does, plus the running
 *  add/del tallies. Snippet line numbers start at 1 on each side. */
export function diffPair(oldText: string, newText: string): DiffPairResult {
  if (oldText === "" && newText === "") {
    return {
      hunk: {
        old_start: 0,
        old_lines: 0,
        new_start: 0,
        new_lines: 0,
        lines: [],
      },
      adds: 0,
      dels: 0,
    };
  }

  const oldNormalized = withTrailingNewline(oldText);
  const newNormalized = withTrailingNewline(newText);

  const lines: RichDiffLine[] = [];
  let oldNum = 1;
  let newNum = 1;
  let adds = 0;
  let dels = 0;

  if (oldNormalized === newNormalized) {
    // Identical content yields no hunks; surface every line as `equal`
    // so the renderer still shows the snippet.
    for (const content of stripNewline(oldNormalized).split("\n")) {
      lines.push({
        type: "equal",
        old_line_num: oldNum++,
        new_line_num: newNum++,
        content,
      });
    }
  } else {
    // A context window this wide keeps every unchanged line in the hunk
    // (no `@@`-collapsing), so the snippet renders in full and line
    // numbers stay contiguous on both sides.
    const meta = parseDiffFromFile(
      { name: "f", contents: oldNormalized },
      { name: "f", contents: newNormalized },
      { context: Number.MAX_SAFE_INTEGER },
    );

    for (const hunk of meta.hunks) {
      for (const segment of hunk.hunkContent) {
        if (segment.type === "context") {
          for (let k = 0; k < segment.lines; k++) {
            lines.push({
              type: "equal",
              old_line_num: oldNum++,
              new_line_num: newNum++,
              content: stripNewline(
                meta.additionLines[segment.additionLineIndex + k] ?? "",
              ),
            });
          }
        } else {
          for (let k = 0; k < segment.deletions; k++) {
            lines.push({
              type: "delete",
              old_line_num: oldNum++,
              new_line_num: null,
              content: stripNewline(
                meta.deletionLines[segment.deletionLineIndex + k] ?? "",
              ),
            });
            dels += 1;
          }
          for (let k = 0; k < segment.additions; k++) {
            lines.push({
              type: "add",
              old_line_num: null,
              new_line_num: newNum++,
              content: stripNewline(
                meta.additionLines[segment.additionLineIndex + k] ?? "",
              ),
            });
            adds += 1;
          }
        }
      }
    }
  }

  const oldLines = oldNum - 1;
  const newLines = newNum - 1;

  return {
    hunk: {
      old_start: oldLines > 0 ? 1 : 0,
      old_lines: oldLines,
      new_start: newLines > 0 ? 1 : 0,
      new_lines: newLines,
      lines,
    },
    adds,
    dels,
  };
}
