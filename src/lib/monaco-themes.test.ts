import { describe, expect, it, vi } from "vitest"

import {
  configureLanguageValidation,
  defineMonacoThemes,
  MONACO_UNICODE_HIGHLIGHT_OPTIONS,
} from "./monaco-themes"

// Regression guard for issue #329: Monaco boxed ordinary CJK full-width
// punctuation (`：` `；` `，` `！` `？` `（` `）` …) because its unicode-highlight
// feature flags characters that look confusable with / are non-basic ASCII.
// Both mechanisms must stay disabled so Chinese/Japanese prose renders cleanly.
describe("MONACO_UNICODE_HIGHLIGHT_OPTIONS", () => {
  it("disables the mechanisms that box visible CJK punctuation", () => {
    expect(MONACO_UNICODE_HIGHLIGHT_OPTIONS.ambiguousCharacters).toBe(false)
    expect(MONACO_UNICODE_HIGHLIGHT_OPTIONS.nonBasicASCII).toBe(false)
  })

  it("keeps invisible-character highlighting at its default (still useful)", () => {
    // Intentionally untouched: surfacing zero-width / BOM characters never
    // boxes legible text and helps catch copy-paste gremlins.
    expect(MONACO_UNICODE_HIGHLIGHT_OPTIONS.invisibleCharacters).toBeUndefined()
  })
})

// The editor never loads a project's build context (tsconfig / node_modules /
// remote schemas), so Monaco's semantic + schema checks are always false
// positives here. This guards that we turn them off while keeping the
// context-free syntax checks that are still worth showing.
describe("configureLanguageValidation", () => {
  function makeMonaco() {
    const tsDefaults = {
      setCompilerOptions: vi.fn(),
      setDiagnosticsOptions: vi.fn(),
    }
    const jsDefaults = {
      setCompilerOptions: vi.fn(),
      setDiagnosticsOptions: vi.fn(),
    }
    const jsonDefaults = { setDiagnosticsOptions: vi.fn() }
    const monaco = {
      // Enough of the surface for both `configureLanguageValidation` and the
      // full `defineMonacoThemes` (which also registers the diff language, the
      // python tokenizer, and the themes) to run against the same mock.
      languages: {
        getLanguages: () => [] as { id: string }[],
        register: vi.fn(),
        setMonarchTokensProvider: vi.fn(),
        typescript: {
          JsxEmit: { Preserve: 1 },
          ScriptTarget: { ESNext: 99 },
          ModuleKind: { ESNext: 99 },
          ModuleResolutionKind: { NodeJs: 2 },
          typescriptDefaults: tsDefaults,
          javascriptDefaults: jsDefaults,
        },
        json: { jsonDefaults },
      },
      editor: { defineTheme: vi.fn() },
    }
    return { monaco, tsDefaults, jsDefaults, jsonDefaults }
  }

  it("disables TS/JS semantic + suggestion checks but keeps syntax errors", () => {
    const { monaco, tsDefaults, jsDefaults } = makeMonaco()

    configureLanguageValidation(
      monaco as unknown as Parameters<typeof configureLanguageValidation>[0]
    )

    for (const defaults of [tsDefaults, jsDefaults]) {
      const diagnostics = defaults.setDiagnosticsOptions.mock.calls[0][0]
      expect(diagnostics.noSemanticValidation).toBe(true)
      expect(diagnostics.noSuggestionDiagnostics).toBe(true)
      // Genuine malformed-code errors are context-free — still shown.
      expect(diagnostics.noSyntaxValidation).toBe(false)

      // JSX must parse without a compiler-flag diagnostic.
      const compiler = defaults.setCompilerOptions.mock.calls[0][0]
      expect(compiler.jsx).toBe(1)
    }
  })

  it("stops JSON remote-schema fetching but keeps structural validation", () => {
    const { monaco, jsonDefaults } = makeMonaco()

    configureLanguageValidation(
      monaco as unknown as Parameters<typeof configureLanguageValidation>[0]
    )

    const options = jsonDefaults.setDiagnosticsOptions.mock.calls[0][0]
    expect(options.validate).toBe(true)
    expect(options.enableSchemaRequest).toBe(false)
    expect(options.schemaRequest).toBe("ignore")
    expect(options.schemaValidation).toBe("ignore")
  })

  it("is wired into the shared defineMonacoThemes beforeMount hook", () => {
    // All three editor surfaces (file editor, diff viewer, merge editor) mount
    // through `defineMonacoThemes`, so this call is the only thing that carries
    // the validation config onto them. Guard against it being dropped in a
    // refactor — the per-function unit tests above would still pass without it.
    const { monaco, tsDefaults, jsonDefaults } = makeMonaco()

    defineMonacoThemes(
      monaco as unknown as Parameters<typeof defineMonacoThemes>[0]
    )

    expect(tsDefaults.setDiagnosticsOptions).toHaveBeenCalled()
    expect(jsonDefaults.setDiagnosticsOptions).toHaveBeenCalled()
  })
})
