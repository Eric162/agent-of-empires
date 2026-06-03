/**
 * In-diff find that searches the diff *model* (the raw old/new file text),
 * not the rendered DOM.
 *
 * The diff is rendered with a virtualized renderer (`@pierre/diffs`), so
 * off-screen lines are not in the DOM and the browser's native Cmd+F can't
 * reach them. Searching the contents directly lets find/next/prev jump to a
 * match anywhere in the file; the caller then scrolls the renderer to it.
 */

export type FindSide = "old" | "new";

export interface FindMatch {
  /** Which file side (old = deletions/base, new = additions/working). */
  side: FindSide;
  /** 1-based line number within that side. */
  lineNumber: number;
  /** 0-based start char offset within the line. */
  startCol: number;
  /** Exclusive end char offset within the line. */
  endCol: number;
  /** Global ordering index across all returned matches. */
  index: number;
}

export interface FindOptions {
  caseSensitive?: boolean;
  regex?: boolean;
  /**
   * Which sides to search and in what order. Defaults to `["old", "new"]`,
   * so matches are grouped old-side-first, then by line, then by column.
   */
  sides?: FindSide[];
}

/**
 * Find every non-overlapping occurrence of `query` in the old/new contents.
 *
 * Returns an empty array for an empty query. Throws `SyntaxError` when
 * `regex` is set and `query` is not a valid regular expression, so the caller
 * can surface an "invalid pattern" state.
 */
export function findMatches(
  oldContent: string,
  newContent: string,
  query: string,
  opts: FindOptions = {},
): FindMatch[] {
  if (query.length === 0) return [];

  const sides = opts.sides ?? ["old", "new"];
  const matcher = opts.regex
    ? regexMatcher(query, opts.caseSensitive ?? false)
    : literalMatcher(query, opts.caseSensitive ?? false);

  const matches: FindMatch[] = [];
  let index = 0;
  for (const side of sides) {
    const content = side === "old" ? oldContent : newContent;
    // Don't synthesize a trailing empty line for content ending in "\n".
    const lines = splitLines(content);
    for (const [i, line] of lines.entries()) {
      for (const [startCol, endCol] of matcher(line)) {
        matches.push({
          side,
          lineNumber: i + 1,
          startCol,
          endCol,
          index: index++,
        });
      }
    }
  }
  return matches;
}

function splitLines(content: string): string[] {
  if (content.length === 0) return [];
  const lines = content.split("\n");
  // A trailing newline produces a spurious final "" entry; drop it.
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

type LineMatcher = (line: string) => Array<[number, number]>;

function literalMatcher(query: string, caseSensitive: boolean): LineMatcher {
  const needle = caseSensitive ? query : query.toLowerCase();
  return (line) => {
    const hay = caseSensitive ? line : line.toLowerCase();
    const out: Array<[number, number]> = [];
    let from = 0;
    for (;;) {
      const at = hay.indexOf(needle, from);
      if (at === -1) break;
      out.push([at, at + needle.length]);
      // Non-overlapping: advance past this match (needle is non-empty here).
      from = at + needle.length;
    }
    return out;
  };
}

function regexMatcher(query: string, caseSensitive: boolean): LineMatcher {
  // Constructed once; reused per line by resetting lastIndex. Throws
  // SyntaxError on an invalid pattern, which callers catch.
  const flags = caseSensitive ? "g" : "gi";
  const re = new RegExp(query, flags);
  return (line) => {
    const out: Array<[number, number]> = [];
    re.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(line)) !== null) {
      const start = m.index;
      const end = start + m[0].length;
      out.push([start, end]);
      // Guard against zero-width matches (e.g. `a*`) looping forever.
      re.lastIndex = m[0].length === 0 ? re.lastIndex + 1 : end;
    }
    return out;
  };
}
