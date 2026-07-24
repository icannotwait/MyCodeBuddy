import { readFileSync } from "node:fs"
import { resolve } from "node:path"
import { describe, expect, it } from "vitest"

const source = readFileSync(
  resolve(process.cwd(), "src/contexts/workspace-context.tsx"),
  "utf8"
)

describe("workspace-context dirty-close confirmation", () => {
  it("never calls window.confirm (updaters must stay side-effect free)", () => {
    // Blocking confirm inside a setFileTabs updater fired twice under
    // StrictMode's double-invoke and blocked the whole webview. The guard now
    // lives outside the updater and confirms via a provider-owned AlertDialog.
    expect(source).not.toContain("window.confirm")
  })

  it("routes dirty closes through the pendingDirtyClose AlertDialog", () => {
    expect(source).toMatch(/pendingDirtyClose/)
    expect(source).toMatch(/checkDirtyClose/)
    expect(source).toMatch(/AlertDialogContent/)
  })

  it("activates keepTabId after dirty close-others confirm", () => {
    // Regression: confirm used to call closeFileTabsByIdsNow(targetIds) only,
    // which fell back to next[last] when the active tab was among those closed.
    expect(source).toMatch(
      /closeFileTabsByIdsNow\(\s*current\.targetIds\s*,\s*current\.keepTabId\s*\)/
    )
    expect(source).toMatch(/pickActiveAfterBulkClose/)
  })
})
