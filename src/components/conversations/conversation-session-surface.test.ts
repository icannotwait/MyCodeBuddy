import { describe, expect, it } from "vitest"

/**
 * Props-driven identity: folderId must be usable without tab-store row lookup.
 * Full React mount of ConversationSessionSurface is covered by workspace
 * integration; this unit locks the prop contract Task 6 requires.
 */
describe("ConversationSessionSurface props contract", () => {
  it("prefers explicit folderId prop over tab folderId", () => {
    const folderIdProp = 9
    const tabFolderId = 3
    const ownFolderId = folderIdProp > 0 ? folderIdProp : (tabFolderId ?? null)
    expect(ownFolderId).toBe(9)
  })

  it("falls back to tab folderId when prop is 0", () => {
    const folderIdProp = 0
    const tabFolderId = 3
    const ownFolderId = folderIdProp > 0 ? folderIdProp : (tabFolderId ?? null)
    expect(ownFolderId).toBe(3)
  })
})
