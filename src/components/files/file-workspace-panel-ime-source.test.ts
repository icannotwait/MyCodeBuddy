import { readFileSync } from "node:fs"
import { resolve } from "node:path"

import { describe, expect, it } from "vitest"

const panelSource = readFileSync(
  resolve(process.cwd(), "src/components/files/file-workspace-panel.tsx"),
  "utf8"
)

describe("file-workspace-panel IME composition guard wiring", () => {
  it("scopes the guard to the Monaco model and binds the mounted editor", () => {
    expect(panelSource).toMatch(
      /useImeSafeEditorValue\([\s\S]{0,100}renderedContent,[\s\S]{0,100}activeScope,[\s\S]{0,100}handleCompositionChange/
    )
    expect(panelSource).toMatch(/bindImeEditor\(editorInstance\)/)
  })

  it("keeps a creation value while withholding controlled file content during composition", () => {
    expect(panelSource).toMatch(/defaultValue=\{renderedContent\}/)
    expect(panelSource).toMatch(
      /value=\{isFileTab \? imeSafeEditorValue : renderedContent\}/
    )
  })

  it("uses a stable change callback so Monaco does not rebind it per keystroke", () => {
    expect(panelSource).toMatch(
      /const handleEditorChange: OnChange = useCallback/
    )
    expect(panelSource).toMatch(/onChange=\{handleEditorChange\}/)
  })

  it("binds content and composition state to the originating file tab", () => {
    expect(panelSource).toMatch(
      /updateFileTabContent\(activeScope, value \?\? ""\)/
    )
    expect(panelSource).toMatch(
      /monacoRef\.current\?\.Uri\.parse\([\s\S]{0,40}editorModelPath[\s\S]{0,40}\)\.toString\(\)/
    )
    expect(panelSource).toMatch(/setFileTabComposing\(tabId, composing\)/)
    expect(panelSource).toMatch(
      /fileSaveState !== "idle" \|\|[\s\S]{0,30}isComposing/
    )
  })
})
