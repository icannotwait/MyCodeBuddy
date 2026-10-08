import type { CSSProperties } from "react"
import type { RoundtableConfig } from "@/lib/roundtable/types"

/**
 * Brand accent for a roundtable speaker, keyed by the agent behind it (the
 * same key that picks its icon), so a color always matches the mark next to
 * it. `color` tints rings, bars and avatar fills; `text` is the darker/lighter
 * shade used for names and chips. Every `text`/`textDark` keeps at least 4.5:1
 * contrast on white/#f5f5f5 and on #0a0a0a/#262626 respectively.
 */
export interface RoundtableBrand {
  label: string
  color: string
  colorDark: string
  text: string
  textDark: string
}

export const ROUNDTABLE_BRANDS = {
  // xAI Grok: black mark; near-white in dark mode so it stays visible.
  grok: {
    label: "Grok",
    color: "#111111",
    colorDark: "#E5E5E5",
    text: "#111111",
    textDark: "#E5E5E5",
  },
  // Google Antigravity runs Gemini: Google blue.
  antigravity: {
    label: "Antigravity",
    color: "#1A73E8",
    colorDark: "#8AB4F8",
    text: "#1967D2",
    textDark: "#8AB4F8",
  },
  // Gemini CLI: Gemini deep blue.
  gemini: {
    label: "Gemini",
    color: "#3186FF",
    colorDark: "#7CACF8",
    text: "#1F5FD6",
    textDark: "#A8C7FA",
  },
  // Anthropic / Claude Code orange.
  claude_code: {
    label: "Claude",
    color: "#D97757",
    colorDark: "#E0896B",
    text: "#B5502D",
    textDark: "#EBA085",
  },
  // OpenAI / Codex light blue.
  codex: {
    label: "OpenAI",
    color: "#7A9DFF",
    colorDark: "#9DB7FF",
    text: "#3B63E0",
    textDark: "#A8C0FF",
  },
  open_code: {
    label: "OpenCode",
    color: "#3F3F46",
    colorDark: "#A1A1AA",
    text: "#3F3F46",
    textDark: "#D4D4D8",
  },
  cline: {
    label: "Cline",
    color: "#8B5CF6",
    colorDark: "#A78BFA",
    text: "#6D28D9",
    textDark: "#C4B5FD",
  },
  hermes: {
    label: "Hermes",
    color: "#F59E0B",
    colorDark: "#FBBF24",
    text: "#B45309",
    textDark: "#FCD34D",
  },
  code_buddy: {
    label: "CodeBuddy",
    color: "#0052D9",
    colorDark: "#5C8DFF",
    text: "#0052D9",
    textDark: "#8FB0FF",
  },
  kimi_code: {
    label: "Kimi",
    color: "#1783FF",
    colorDark: "#5AA6FF",
    text: "#0866D6",
    textDark: "#8CC2FF",
  },
  pi: {
    label: "Pi",
    color: "#0D9488",
    colorDark: "#2DD4BF",
    text: "#0F766E",
    textDark: "#5EEAD4",
  },
  cursor: {
    label: "Cursor",
    color: "#27272A",
    colorDark: "#D4D4D8",
    text: "#27272A",
    textDark: "#E4E4E7",
  },
  deepseek: {
    label: "DeepSeek",
    color: "#4D6BFE",
    colorDark: "#8C9EFF",
    text: "#3451E6",
    textDark: "#A9B7FF",
  },
  qoder: {
    label: "Qoder",
    color: "#6C4CF1",
    colorDark: "#A08CFF",
    text: "#5636D8",
    textDark: "#BBAEFF",
  },
  // Only reachable through a model hint (no built-in Qwen agent icon).
  qwen: {
    label: "Qwen",
    color: "#615CED",
    colorDark: "#A29EFF",
    text: "#4A44D6",
    textDark: "#BDB9FF",
  },
} as const satisfies Record<string, RoundtableBrand>

export type RoundtableBrandKey = keyof typeof ROUNDTABLE_BRANDS | "neutral"

export const NEUTRAL_BRAND: RoundtableBrand = {
  label: "",
  color: "#71717A",
  colorDark: "#A1A1AA",
  text: "#52525B",
  textDark: "#D4D4D8",
}

const AGENT_ALIASES: Record<string, keyof typeof ROUNDTABLE_BRANDS> = {
  claude: "claude_code",
  "claude-code": "claude_code",
  anthropic: "claude_code",
  openai: "codex",
  gpt: "codex",
  xai: "grok",
  "gemini-cli": "gemini",
  google: "gemini",
  opencode: "open_code",
  "code-buddy": "code_buddy",
  kimi: "kimi_code",
  moonshot: "kimi_code",
}

/** Model/provider hints, used only when the agent itself is unknown. */
const MODEL_HINTS: [RegExp, keyof typeof ROUNDTABLE_BRANDS][] = [
  [/grok|xai/i, "grok"],
  [/claude|anthropic|opus|sonnet|haiku/i, "claude_code"],
  [/antigravity/i, "antigravity"],
  [/gemini|google/i, "gemini"],
  [/deepseek/i, "deepseek"],
  [/kimi|moonshot/i, "kimi_code"],
  [/qwen|tongyi/i, "qwen"],
  [/gpt|openai|codex|(^|[^a-z])o[1-9]([^a-z]|$)/i, "codex"],
]

function isBrandKey(value: string): value is keyof typeof ROUNDTABLE_BRANDS {
  return Object.hasOwn(ROUNDTABLE_BRANDS, value)
}

/**
 * Brand for an agent, falling back to a model/provider hint (custom agents),
 * then to a neutral gray.
 */
export function roundtableBrandKey(
  agent: string | null | undefined,
  hints: (string | null | undefined)[] = []
): RoundtableBrandKey {
  const normalized = (agent ?? "").trim().toLowerCase()
  if (normalized && isBrandKey(normalized)) return normalized
  if (normalized && AGENT_ALIASES[normalized]) return AGENT_ALIASES[normalized]
  for (const hint of [normalized, ...hints]) {
    if (!hint) continue
    for (const [pattern, key] of MODEL_HINTS) if (pattern.test(hint)) return key
  }
  return "neutral"
}

export function roundtableBrand(key: RoundtableBrandKey): RoundtableBrand {
  return key === "neutral" ? NEUTRAL_BRAND : ROUNDTABLE_BRANDS[key]
}

function mixHex(color: string, target: string, amount: number): string {
  const parse = (hex: string) =>
    [1, 3, 5].map((index) => parseInt(hex.slice(index, index + 2), 16))
  const from = parse(color)
  const to = parse(target)
  return `#${from
    .map((value, index) =>
      Math.round(value + (to[index] - value) * amount)
        .toString(16)
        .padStart(2, "0")
    )
    .join("")
    .toUpperCase()}`
}

/**
 * Shade a brand for the n-th member sharing the same agent: the hue stays,
 * later seats move toward the page background (lighter in light mode, darker
 * in dark mode). Text shades are kept so contrast never drops.
 */
export function shadeRoundtableBrand(
  brand: RoundtableBrand,
  variant: number
): RoundtableBrand {
  if (variant <= 0) return brand
  const amount = Math.min(0.3 * variant, 0.6)
  return {
    ...brand,
    color: mixHex(brand.color, "#FFFFFF", amount),
    colorDark: mixHex(brand.colorDark, "#000000", amount),
  }
}

export interface SeatBrand {
  key: RoundtableBrandKey
  /** 0 for the first member using this agent, 1 for the second, … */
  variant: number
  /** True when another member in the room uses the same agent brand. */
  shared: boolean
  brand: RoundtableBrand
}

/** CSS variables consumed by the `--rt-*` utility classes. */
export function brandStyle(brand: RoundtableBrand): CSSProperties {
  return {
    "--rt-c": brand.color,
    "--rt-c-dark": brand.colorDark,
    "--rt-t": brand.text,
    "--rt-t-dark": brand.textDark,
  } as CSSProperties
}

/** Brand per configured participant ordinal, with same-agent shading. */
export function roundtableSeatBrands(
  config: Pick<RoundtableConfig, "participants"> | null | undefined
): Map<number, SeatBrand> {
  const seats = new Map<number, SeatBrand>()
  if (!config) return seats
  const ordered = [...config.participants].sort(
    (left, right) => left.ordinal - right.ordinal
  )
  const keys = ordered.map((participant) =>
    roundtableBrandKey(participant.agent || "codex", [
      participant.model,
      participant.provider_ref,
    ])
  )
  const totals = new Map<RoundtableBrandKey, number>()
  for (const key of keys) totals.set(key, (totals.get(key) ?? 0) + 1)
  const seen = new Map<RoundtableBrandKey, number>()
  ordered.forEach((participant, index) => {
    const key = keys[index]
    const variant = seen.get(key) ?? 0
    seen.set(key, variant + 1)
    seats.set(participant.ordinal, {
      key,
      variant,
      shared: (totals.get(key) ?? 0) > 1,
      brand: shadeRoundtableBrand(roundtableBrand(key), variant),
    })
  })
  return seats
}
