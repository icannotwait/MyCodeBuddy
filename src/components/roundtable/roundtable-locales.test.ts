import { describe, expect, it } from "vitest"
import ar from "@/i18n/messages/ar.json"
import de from "@/i18n/messages/de.json"
import en from "@/i18n/messages/en.json"
import es from "@/i18n/messages/es.json"
import fr from "@/i18n/messages/fr.json"
import ja from "@/i18n/messages/ja.json"
import ko from "@/i18n/messages/ko.json"
import pt from "@/i18n/messages/pt.json"
import zhCN from "@/i18n/messages/zh-CN.json"
import zhTW from "@/i18n/messages/zh-TW.json"

const newMessages = [
  "retryLoad",
  "retryProviders",
  "loadingRooms",
  "noRooms",
  "discussions",
  "refreshFailed",
  "liveUpdatesUnavailable",
  "phaseProposal",
  "phaseCritique",
  "phaseCritiqueRound",
  "phaseSynthesis",
  "phaseUnknown",
  "conclusion",
  "claims",
  "responses",
  "openQuestions",
  "positionChanges",
  "recommendation",
  "alternatives",
  "consensus",
  "disagreements",
  "risks",
  "decisionRequests",
  "coverage",
  "coverageCounts",
  "confidenceLow",
  "confidenceMedium",
  "confidenceHigh",
  "stanceSupport",
  "stanceChallenge",
  "stanceClarify",
  "stanceRevise",
  "priorityNormal",
  "priorityCritical",
  "agreementExplicit",
  "agreementCompatible",
  "agreementUnresolved",
  "supportedBy",
  "inference",
  "attemptBadge",
  "attemptId",
  "abstain",
  "reason",
  "messageId",
  "bodyHash",
  "speakerId",
  "phaseId",
  "publishedSeq",
  "evidenceAliases",
]

describe("roundtable loading and retry translations", () => {
  it.each(Object.entries({ ar, de, en, es, fr, ja, ko, pt, zhCN, zhTW }))(
    "includes translated loading and recovery copy in %s",
    (locale, messages) => {
      const roundtable: Record<string, string> = messages.Roundtable
      for (const key of newMessages) {
        expect(roundtable[key], `${locale}.${key}`).toEqual(expect.any(String))
        expect(roundtable[key].trim(), `${locale}.${key}`).not.toBe("")
        if (locale !== "en" && key !== "discussions") {
          const english: Record<string, string> = en.Roundtable
          expect(roundtable[key], `${locale}.${key}`).not.toBe(english[key])
        }
      }
    }
  )
})
