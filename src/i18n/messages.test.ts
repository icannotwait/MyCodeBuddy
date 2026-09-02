import { readFileSync } from "node:fs"
import { resolve } from "node:path"
import { createTranslator } from "next-intl"
import {
  isObjectLiteralExpression,
  isPropertyAssignment,
  isStringLiteral,
  parseJsonText,
  type Node,
} from "typescript"
import { describe, expect, it } from "vitest"

import ar from "./messages/ar.json"
import de from "./messages/de.json"
import en from "./messages/en.json"
import es from "./messages/es.json"
import fr from "./messages/fr.json"
import ja from "./messages/ja.json"
import ko from "./messages/ko.json"
import pt from "./messages/pt.json"
import zhCN from "./messages/zh-CN.json"
import zhTW from "./messages/zh-TW.json"

type MessageNode = string | { [key: string]: MessageNode }

function collectKeys(node: MessageNode, prefix = ""): string[] {
  if (typeof node === "string") {
    return [prefix]
  }
  const out: string[] = []
  for (const [key, value] of Object.entries(node)) {
    const next = prefix ? `${prefix}.${key}` : key
    out.push(...collectKeys(value, next))
  }
  return out
}

const reference = new Set(collectKeys(en as MessageNode))
const locales = [ar, de, en, es, fr, ja, ko, pt, zhCN, zhTW] as const
const localeFiles = [
  "ar.json",
  "de.json",
  "en.json",
  "es.json",
  "fr.json",
  "ja.json",
  "ko.json",
  "pt.json",
  "zh-CN.json",
  "zh-TW.json",
] as const

function collectDuplicateKeys(filename: string): string[] {
  const text = readFileSync(resolve("src/i18n/messages", filename), "utf8")
  const source = parseJsonText(filename, text)
  const duplicates: string[] = []

  function visit(node: Node, path: string) {
    if (!isObjectLiteralExpression(node)) {
      node.forEachChild((child) => visit(child, path))
      return
    }

    const seen = new Set<string>()
    for (const property of node.properties) {
      if (!isPropertyAssignment(property) || !isStringLiteral(property.name)) {
        continue
      }
      const key = property.name.text
      const nextPath = path ? `${path}.${key}` : key
      if (seen.has(key)) duplicates.push(nextPath)
      seen.add(key)
      visit(property.initializer, nextPath)
    }
  }

  source.forEachChild((node) => visit(node, ""))
  return duplicates
}

// `en.json` is the source of truth. Any missing key in another locale fails
// the test with the exact dotted path, making translation gaps grep-able.
describe("i18n locale key parity vs en.json", () => {
  it.each(localeFiles)("%s has no duplicate message keys", (filename) => {
    expect(collectDuplicateKeys(filename)).toEqual([])
  })

  it.each([
    ["ar", ar],
    ["de", de],
    ["es", es],
    ["fr", fr],
    ["ja", ja],
    ["ko", ko],
    ["pt", pt],
    ["zh-CN", zhCN],
    ["zh-TW", zhTW],
  ] as const)("%s has the same key set as en", (_locale, messages) => {
    const localeKeys = new Set(collectKeys(messages as MessageNode))
    const missing = [...reference].filter((k) => !localeKeys.has(k))
    const extra = [...localeKeys].filter((k) => !reference.has(k))
    expect({ missing, extra }).toEqual({ missing: [], extra: [] })
  })

  it("defines v2 workflow controls and historical links", () => {
    for (const messages of locales) {
      const workflow = (
        messages as unknown as {
          Folder: { chat: { workflowGraph: Record<string, string> } }
        }
      ).Folder.chat.workflowGraph

      expect(workflow.laneToggleAria).toContain("{phase}")
      expect(workflow.dependenciesToggle).toContain("{count}")
      expect(workflow.moreCurrentNodes).toContain("{count}")
      expect(workflow.phaseProgressAria).toContain("{phase}")
      expect(workflow.phaseProgressAria).toContain("{status}")
      expect(workflow.phaseProgressAria).toContain("{progress}")

      for (const key of [
        "completionResolved",
        "completionNeedsDecision",
        "completionBlocked",
        "completionRetryArtifact",
        "completionLegacyReadOnly",
        "completionLegacySource",
        "completionLegacySuccessor",
        "completionAutomaticWake",
        "completionStale",
        "completionConflict",
      ]) {
        expect(workflow[key], `missing workflow completion key ${key}`).toEqual(
          expect.any(String)
        )
      }
    }
  })

  it("defines Simple DAG copy and relationship placeholders", () => {
    const dagKeys = [
      "dagAria",
      "dagSelectedNode",
      "dagCurrentNode",
      "dagDependsOn",
      "dagRequiredBy",
      "dagInvalidGraph",
      "dagFallbackAria",
    ] as const

    for (const messages of locales) {
      const workflow = (
        messages as unknown as {
          Folder: { chat: { workflowGraph: Record<string, string> } }
        }
      ).Folder.chat.workflowGraph

      for (const key of dagKeys) {
        expect(workflow[key], `missing workflow DAG key ${key}`).toEqual(
          expect.any(String)
        )
        expect(workflow[key].trim(), `empty workflow DAG key ${key}`).not.toBe(
          ""
        )
      }
      expect(
        Object.keys(workflow)
          .filter((key) => key.startsWith("dag"))
          .sort()
      ).toEqual([...dagKeys].sort())
      expect(workflow.dagDependsOn).toContain("{nodes}")
      expect(workflow.dagRequiredBy).toContain("{nodes}")
    }
  })

  it("defines restart-required pop-out copy in all ten locales", () => {
    for (const messages of locales) {
      const popout = messages.ConversationPopout as Record<string, string>
      for (const key of [
        "runtimeRestartRequired",
        "restartDrawCode",
        "restartFailed",
      ]) {
        expect(popout[key], `missing ConversationPopout.${key}`).toEqual(
          expect.any(String)
        )
        expect(popout[key].trim()).not.toBe("")
      }
    }
    expect(en.ConversationPopout.runtimeRestartRequired).toBe(
      "WebView2 was updated. Restart DrawCode before using conversation pop-out. Restarting interrupts currently running tasks."
    )
  })
})

describe("DrawCode branding", () => {
  it("uses the fork name in every science settings description", () => {
    for (const messages of locales) {
      expect(messages.ScienceSettings.description).toContain("DrawCode")
      expect(messages.ScienceSettings.description).not.toMatch(/\bcodeg\b/i)
    }
  })
})

const ALL_LOCALES = [
  ["en", en],
  ["ar", ar],
  ["de", de],
  ["es", es],
  ["fr", fr],
  ["ja", ja],
  ["ko", ko],
  ["pt", pt],
  ["zh-CN", zhCN],
  ["zh-TW", zhTW],
] as const

// Every message goes through ICU MessageFormat, which reserves `<tag>`, `{`,
// `}` and `#`. A string like `<QODER_CONFIG_DIR>/settings.json` parses as an
// unclosed tag and falls back to rendering the KEY — visible in the UI as
// `qoder.configDescription`, and only in the one locale that has it.
//
// The check runs the real production path (`createTranslator`) rather than a
// regex, so it fails on exactly what users would see fail, and it fails with
// the dotted path so the offending string is grep-able.
describe("i18n messages are valid ICU", () => {
  it.each(ALL_LOCALES)("%s renders every message", (locale, messages) => {
    const broken: string[] = []
    const t = createTranslator({
      locale,
      messages: messages as Record<string, MessageNode>,
      onError: () => {},
      // A message that fails to parse reaches here with its own key; anything
      // that needs real ICU arguments renders as its key too, which is fine —
      // we only care that the parse itself succeeded.
      getMessageFallback: ({ key, error }) => {
        if (error?.code === "INVALID_MESSAGE") broken.push(key)
        return key
      },
    })
    for (const key of collectKeys(messages as MessageNode)) {
      // Placeholders are supplied loosely: an unknown-argument error is not an
      // ICU syntax problem, and `getMessageFallback` only records the syntax one.
      t(key as never, { count: 1, name: "x", value: "x" } as never)
    }
    expect(broken).toEqual([])
  })
})
