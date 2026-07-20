/**
 * Pure helpers for on-demand document translation (eligibility, hashing,
 * locale wire ids, naming). Backend remains authoritative for size limits
 * and agent admission.
 */

import {
  fromIntlLocale,
  isAppLocale,
  isIntlLocale,
  mapLocaleTagToAppLocale,
  type IntlLocale,
} from "@/lib/i18n"
import { isImageFile, isOfficePreviewable } from "@/lib/language-detect"
import type { AppLocale } from "@/lib/types"

/** Backend-authoritative max input size (Unicode scalars). Duplicated for FE pre-check UX. */
export const MAX_INPUT_SCALARS = 24_000

export type DocumentTranslateFormat = "markdown" | "plainText"

export type TranslationTransientMeta = {
  type: "translation"
  sourceTabId: string
  sourcePath: string | null
  sourceContentHash: string
  locale: string
  format: DocumentTranslateFormat
  suggestedName: string
}

/** Minimal tab shape for eligibility checks (avoids coupling to full workspace type). */
export type TranslationEligibilityTab = {
  kind: string
  loading: boolean
  path: string | null
  title?: string
  content: string
  language?: string
  transient?: { type: string } | null
}

const TRANSLATABLE_EXTENSIONS = new Set(["md", "markdown", "txt"])

function basenameOf(pathOrName: string): string {
  const normalized = pathOrName.replace(/\\/g, "/")
  const parts = normalized.split("/")
  return parts[parts.length - 1] || pathOrName
}

function extensionOf(pathOrName: string): string {
  const base = basenameOf(pathOrName).toLowerCase()
  const dot = base.lastIndexOf(".")
  if (dot <= 0) return ""
  return base.slice(dot + 1)
}

/**
 * True when the path/name ends with `.md`, `.markdown`, or `.txt`
 * (case-insensitive). Does not accept `.mdx`.
 */
export function isTranslatablePath(path: string | null | undefined): boolean {
  if (!path) return false
  return TRANSLATABLE_EXTENSIONS.has(extensionOf(path))
}

/**
 * Whether the Translate toolbar action should be shown for this tab.
 * Agent configuration is separate: button stays visible when agent is off.
 */
export function isTranslationEligible(tab: TranslationEligibilityTab): boolean {
  if (tab.kind !== "file") return false
  if (tab.loading) return false
  if (tab.transient?.type === "translation") return false
  if (!tab.content.trim()) return false

  const pathOrTitle = tab.path ?? tab.title ?? null
  if (!isTranslatablePath(pathOrTitle)) return false

  if (tab.path) {
    if (isImageFile(tab.path) || isOfficePreviewable(tab.path)) return false
  }

  const language = tab.language?.toLowerCase()
  if (language === "image" || language === "office") return false

  return true
}

/**
 * djb2 hash over UTF-16 code units (`String.charCodeAt`), returned as
 * lowercase hex without padding. Used for source content fingerprinting on
 * transient translation tabs — not a cryptographic hash.
 */
export function hashDocumentContent(s: string): string {
  let hash = 5381
  for (let i = 0; i < s.length; i++) {
    hash = (hash << 5) + hash + s.charCodeAt(i)
    hash = hash >>> 0
  }
  return hash.toString(16)
}

/**
 * Map a next-intl locale tag (or AppLocale wire id) to the snake_case wire
 * id accepted by `translate_document` / `parse_supported_app_locale`.
 * Unknown tags fall back to `en`.
 */
export function intlLocaleToWire(locale: string): AppLocale {
  if (isAppLocale(locale)) return locale
  if (isIntlLocale(locale)) return fromIntlLocale(locale as IntlLocale)
  return mapLocaleTagToAppLocale(locale) ?? "en"
}

export function formatFromTranslatablePath(
  path: string | null | undefined
): DocumentTranslateFormat {
  const ext = path ? extensionOf(path) : ""
  if (ext === "txt") return "plainText"
  return "markdown"
}

/**
 * Default Save-as relative name: `{stem}.{localeWire}{ext}` e.g. README.zh_cn.md
 */
export function buildSuggestedTranslationName(
  sourceName: string,
  localeWire: string
): string {
  const base = basenameOf(sourceName)
  const dot = base.lastIndexOf(".")
  if (dot <= 0) {
    return `${base}.${localeWire}`
  }
  const stem = base.slice(0, dot)
  const ext = base.slice(dot)
  return `${stem}.${localeWire}${ext}`
}

export function buildTranslationTabId(
  sourceTabId: string,
  locale: string,
  requestGen: number
): string {
  return `translate:${sourceTabId}:${locale}:${requestGen}`
}
