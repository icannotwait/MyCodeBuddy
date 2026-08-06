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

// `en.json` is the source of truth. Any missing key in another locale fails
// the test with the exact dotted path, making translation gaps grep-able.
describe("i18n locale key parity vs en.json", () => {
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

  it("defines workflow overlay controls with stable ICU placeholders", () => {
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
        "completionLegacyRestart",
        "completionManualRootResume",
        "completionStale",
        "completionConflict",
      ]) {
        expect(workflow[key], `missing workflow completion key ${key}`).toBe(
          expect.any(String)
        )
      }
    }
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
