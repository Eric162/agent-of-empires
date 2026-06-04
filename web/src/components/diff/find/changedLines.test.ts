import { describe, it, expect } from "vitest";
import { processFile } from "@pierre/diffs";
import { changedLines } from "./changedLines";

// The exact unified-diff format the Rust backend emits (`similar` crate,
// `unified_diff().context_radius(3).header("a/<path>", "b/<path>")`). This is
// the contract the client-side patch parsing depends on.
const OLD = "line 1\nline 2\nline 3\n";
const NEW = "line 1 modified\nline 2\nline 3\nnew line 4\n";
const PATCH = `--- a/test.txt
+++ b/test.txt
@@ -1,3 +1,4 @@
-line 1
+line 1 modified
 line 2
 line 3
+new line 4
`;

describe("processFile on a similar-crate patch", () => {
  it("parses hunks and grafts the full contents", () => {
    const meta = processFile(PATCH, {
      oldFile: { name: "test.txt", contents: OLD },
      newFile: { name: "test.txt", contents: NEW },
    });
    expect(meta).toBeTruthy();
    expect(meta!.hunks).toHaveLength(1);
    // Full contents grafted: deletion/addition lines cover the whole file,
    // not just the patch, so hunk expansion and find line numbers are
    // file-accurate.
    expect(meta!.deletionLines).toEqual(["line 1\n", "line 2\n", "line 3\n"]);
    expect(meta!.additionLines).toEqual([
      "line 1 modified\n",
      "line 2\n",
      "line 3\n",
      "new line 4\n",
    ]);
  });

  it("changedLines maps to file-accurate line numbers", () => {
    const meta = processFile(PATCH, {
      oldFile: { name: "test.txt", contents: OLD },
      newFile: { name: "test.txt", contents: NEW },
    })!;
    expect(changedLines(meta)).toEqual([
      { side: "old", lineNumber: 1, text: "line 1" },
      { side: "new", lineNumber: 1, text: "line 1 modified" },
      { side: "new", lineNumber: 4, text: "new line 4" },
    ]);
  });
});
