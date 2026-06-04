// Large-diff handling. Pierre computes the diff, inline word-diffs, and
// tokenizes the whole file in memory regardless of virtualization, so a big
// lockfile churn can hang or crash the tab. The viewer refuses to inline-render
// pathological diffs (placeholder) and degrades merely-large ones.
import { test, expect } from "./helpers/mockedTest";
import type { Page } from "@playwright/test";
import { clickSidebarSession } from "./helpers/sidebar";
import { mockTerminalApis } from "./helpers/terminal-mocks";

// Realistic lockfile-ish lines (long, distinct), entirely different old vs new.
function lockLines(n: number, salt: string): string {
  return (
    Array.from(
      { length: n },
      (_, i) =>
        `  /@scope/pkg-${salt}-${i}@${(i % 9) + 1}.${i % 20}.${i % 7}: ` +
        `resolution: {integrity: sha512-${salt}${"abc123".repeat(8)}${i}}`,
    ).join("\n") + "\n"
  );
}

async function mount(page: Page, adds: number, dels: number) {
  await mockTerminalApis(page);
  await page.route("**/api/sessions/*/diff/files", (r) =>
    r.fulfill({
      json: {
        files: [
          {
            path: "pnpm-lock.yaml",
            old_path: null,
            status: "modified",
            additions: adds,
            deletions: dels,
          },
        ],
        per_repo_bases: [{ base_branch: "main" }],
        warning: null,
      },
    }),
  );
  await page.route(/\/api\/sessions\/[^/]+\/diff\/file\?/, (r) =>
    r.fulfill({
      json: {
        file: {
          path: "pnpm-lock.yaml",
          old_path: null,
          status: "modified",
          additions: adds,
          deletions: dels,
        },
        old_content: lockLines(dels, "old"),
        new_content: lockLines(adds, "new"),
        is_binary: false,
        truncated: false,
      },
    }),
  );
  await page.goto("/");
  await clickSidebarSession(page, "pinch-test");
}

test.describe("Large diff handling", () => {
  test("huge diff shows a placeholder instead of crashing", async ({ page }) => {
    await mount(page, 10000, 13000); // ~23k changed lines
    await page.getByText("pnpm-lock.yaml").first().click();
    await expect(
      page.getByText(/Large diff not rendered inline/).first(),
    ).toBeVisible({ timeout: 10000 });
    // Pierre is never mounted for the huge case.
    await expect(page.locator("diffs-container")).toHaveCount(0);
  });

  test("merely-large diff still renders (degraded)", async ({ page }) => {
    await mount(page, 4000, 4000);
    await page.getByText("pnpm-lock.yaml").first().click();
    await expect(page.locator("diffs-container").first()).toBeVisible({
      timeout: 30000,
    });
  });
});
