import { readFileSync } from "node:fs"
import { resolve } from "node:path"
import { describe, expect, it } from "vitest"

const read = (p: string) => readFileSync(resolve(process.cwd(), p), "utf8")

const listSource = read(
  "src/components/conversations/sidebar-conversation-list.tsx"
)
const headerSource = read(
  "src/components/conversations/conversation-detail-header.tsx"
)
const controllerSource = read(
  "src/components/layout/workspace-chrome-controller.tsx"
)

describe("conversation mutation failure feedback", () => {
  it("rolls back and toasts on failed optimistic status/pin updates", () => {
    for (const source of [listSource, headerSource]) {
      expect(source).toContain("statusChangeFailed")
      expect(source).toContain("pinFailed")
    }
  })

  it("toasts rename/delete failures instead of dying silently", () => {
    expect(headerSource).toContain("renameFailed")
    expect(headerSource).toContain("deleteFailed")
  })

  it("toasts open-folder failures in the workspace folder dialog", () => {
    const dialogSource = read(
      "src/components/layout/workspace-folder-dialog.tsx"
    )
    expect(dialogSource).toContain("openFailed")
    expect(dialogSource).toContain("toast.error")
    expect(listSource).toContain("WorkspaceFolderDialog")
    expect(controllerSource).toContain("WorkspaceFolderDialog")
  })

  it("prevents Radix AlertDialogAction auto-close on delete confirm", () => {
    // Without preventDefault, async failure cannot keep the dialog open.
    expect(headerSource).toMatch(/preventDefault/)
    const cardSource = read(
      "src/components/conversations/sidebar-conversation-card.tsx"
    )
    expect(cardSource).toMatch(/preventDefault/)
  })
})
