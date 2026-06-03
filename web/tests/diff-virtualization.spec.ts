import { test, expect } from "./helpers/mockedTest";
import { clickSidebarSession } from "./helpers/sidebar";
import { mockTerminalApis } from "./helpers/terminal-mocks";
import type { Page } from "@playwright/test";

// The @pierre/diffs renderer virtualizes large diffs: off-screen rows are not
// in the DOM until scrolled near. This is the core perf win, so we assert both
// halves of it — a late row is absent on first paint, and appears after
// scrolling the viewer — plus that the in-diff find bar can jump to an
// off-screen match (which native Cmd+F can't reach under virtualization).

const LINES = 1000;
// Mostly-shared context with every 10th line changed, so changed lines (the
// ones the assertions key on) are distributed throughout the file rather than
// bunched at one end the way an all-different file would diff.
function bigContent(prefix: string): string {
  return (
    Array.from({ length: LINES }, (_, i) =>
      i % 10 === 0 ? `${prefix} ${i + 1}: changed` : `shared line ${i + 1}`,
    ).join("\n") + "\n"
  );
}

const DIFF_FILES_RESPONSE = {
  files: [
    { path: "big.txt", old_path: null, status: "modified", additions: LINES, deletions: LINES },
  ],
  per_repo_bases: [{ base_branch: "main" }],
  warning: null,
};

const DIFF_FILE_RESPONSE = {
  file: { path: "big.txt", old_path: null, status: "modified", additions: LINES, deletions: LINES },
  old_content: bigContent("base"),
  new_content: bigContent("edit"),
  is_binary: false,
  truncated: false,
};

async function setup(page: Page) {
  await mockTerminalApis(page);
  await page.route("**/api/sessions/*/diff/files", (r) =>
    r.fulfill({ json: DIFF_FILES_RESPONSE }),
  );
  await page.route(/\/api\/sessions\/[^/]+\/diff\/file\?/, (r) =>
    r.fulfill({ json: DIFF_FILE_RESPONSE }),
  );
}

async function openBigFile(page: Page) {
  await page.goto("/");
  await expect(page.locator("header")).toBeVisible();
  await clickSidebarSession(page, "pinch-test");
  await expect(page.getByText("big.txt").first()).toBeVisible({ timeout: 10000 });
  await page.getByText("big.txt").first().click();
  // Early rows render.
  await expect(page.getByText("edit 1:", { exact: false }).first()).toBeVisible({
    timeout: 15000,
  });
}

test.use({ viewport: { width: 1280, height: 720 } });

test.describe("Diff virtualization", () => {
  test("late rows mount only after scrolling (virtualized)", async ({ page }) => {
    await setup(page);
    await openBigFile(page);

    // A row deep in the file is not in the DOM on first paint.
    await expect(page.getByText("edit 981:", { exact: false })).toHaveCount(0);

    // Scroll the diff viewer's scroll container to the bottom in steps; the
    // virtualizer mounts rows as they approach the viewport.
    await page.evaluate(() => {
      const host = document.querySelector("diffs-container");
      // Walk up to the nearest scrollable ancestor.
      let el = host?.parentElement as HTMLElement | null;
      while (el && el.scrollHeight <= el.clientHeight) el = el.parentElement;
      if (el) el.scrollTop = el.scrollHeight;
    });

    await expect(page.getByText("edit 991:", { exact: false }).first()).toBeVisible({
      timeout: 15000,
    });
  });

  test("find jumps to an off-screen match", async ({ page }) => {
    await setup(page);
    await openBigFile(page);

    // Open find and search for a token only present deep in the file.
    await page.getByRole("button", { name: "Find in diff" }).click();
    const input = page.getByRole("textbox", { name: "Find in diff" });
    await input.fill("edit 971:");
    // Match count surfaces (found via model search, not the DOM).
    await expect(page.getByText(/\/\d+$/).first()).toBeVisible();
    // The match scrolls into view.
    await expect(page.getByText("edit 971:", { exact: false }).first()).toBeVisible({
      timeout: 15000,
    });
  });
});
