import { render } from "@testing-library/react"
import { describe, expect, it } from "vitest"

import { ReferenceBadge } from "./reference-badge"
import type { ReferenceAttrs } from "../types"

const BASELINE_REFERENCE_TEXT =
  "2026-07-06-simple-packaging-storage-ballistic-throw-design.md"
const LONG_REFERENCE_TEXT = `${BASELINE_REFERENCE_TEXT}-with-additional-differentiating-suffix.md`

function expectedMiddleTruncate(text: string): string {
  const edgeChars = Math.floor(
    (BASELINE_REFERENCE_TEXT.length - "...".length) / 2
  )
  return text.length <= BASELINE_REFERENCE_TEXT.length
    ? text
    : `${text.slice(0, edgeChars)}...${text.slice(-edgeChars)}`
}

function ref(partial: Partial<ReferenceAttrs>): ReferenceAttrs {
  return {
    refType: "file",
    id: "",
    label: "",
    uri: null,
    meta: null,
    ...partial,
  }
}

/** The badge root carries `data-reference-badge` and `data-ref-type`. */
function badgeOf(container: HTMLElement): HTMLElement {
  const el = container.querySelector<HTMLElement>("[data-reference-badge]")
  if (!el) throw new Error("no reference badge rendered")
  return el
}

describe("ReferenceBadge", () => {
  it("renders the label and is vertically centered (align-middle)", () => {
    const { container } = render(
      <ReferenceBadge data={ref({ refType: "file", label: "app.ts" })} />
    )
    const badge = badgeOf(container)
    expect(badge).toHaveTextContent("app.ts")
    // Task 3: badges sit on the text's middle, not its baseline.
    expect(badge).toHaveClass("align-middle")
    expect(badge).not.toHaveClass("align-baseline")
  })

  it("keeps baseline-length file labels complete and expands the badge width", () => {
    const { container } = render(
      <ReferenceBadge
        data={ref({ refType: "file", label: BASELINE_REFERENCE_TEXT })}
      />
    )
    const badge = badgeOf(container)
    expect(badge).toHaveTextContent(BASELINE_REFERENCE_TEXT)
    expect(badge).toHaveClass("max-w-[36rem]")
  })

  it("middle-shortens longer file labels instead of clipping the suffix", () => {
    const { container } = render(
      <ReferenceBadge
        data={ref({ refType: "file", label: LONG_REFERENCE_TEXT })}
      />
    )
    const badge = badgeOf(container)
    expect(badge).toHaveTextContent(expectedMiddleTruncate(LONG_REFERENCE_TEXT))
    expect(badge).not.toHaveTextContent(LONG_REFERENCE_TEXT)
  })

  it("colors a file reference blue with no background or border", () => {
    const { container } = render(
      <ReferenceBadge data={ref({ refType: "file", label: "app.ts" })} />
    )
    const badge = badgeOf(container)
    expect(badge).toHaveAttribute("data-ref-type", "file")
    // Text-only: a color, but no background / border (req 3).
    expect(badge).toHaveClass("text-blue-700")
    expect(badge.className).not.toMatch(/\bbg-/)
    expect(badge.className).not.toMatch(/\bborder\b/)
    expect(container.querySelector(".lucide-file-text")).not.toBeNull()
  })

  it("colors a session reference emerald with the conversation glyph", () => {
    const { container } = render(
      <ReferenceBadge data={ref({ refType: "session", label: "#42" })} />
    )
    const badge = badgeOf(container)
    expect(badge).toHaveAttribute("data-ref-type", "session")
    expect(badge).toHaveClass("text-emerald-700")
    // A session badge always shows the neutral conversation glyph (not the
    // owning agent's icon, not Hash) — see the `session` case in ReferenceIcon.
    expect(container.querySelector(".lucide-message-square")).not.toBeNull()
    expect(container.querySelector(".lucide-hash")).toBeNull()
  })

  it("shows the conversation glyph and no status dot even with agent/status meta", () => {
    // The inline badge ignores the owning agent and the live status: it never
    // shows an agent icon and never paints the trailing status dot (those belong
    // to the `@`-panel option row and the sidebar, not the inline chip).
    const { container } = render(
      <ReferenceBadge
        data={ref({
          refType: "session",
          label: "Login refactor",
          meta: { agentType: "codex", status: "in_progress" },
        })}
      />
    )
    expect(container.querySelector(".lucide-message-square")).not.toBeNull()
    // No agent icon: AgentIcon for codex renders an <svg> with <title>Codex</title>;
    // the conversation glyph carries no <title>, so none should be present.
    expect(container.querySelector("title")).toBeNull()
    // No trailing status dot (the removed `rounded-full` indicator).
    expect(container.querySelector(".rounded-full")).toBeNull()
  })

  it("renders a command/skill with the command glyph, colored rose", () => {
    const { container } = render(
      <ReferenceBadge
        data={ref({
          refType: "skill",
          id: "build",
          label: "build",
          meta: { invocationPrefix: "/" },
        })}
      />
    )
    const badge = badgeOf(container)
    expect(badge).toHaveAttribute("data-ref-type", "skill")
    expect(badge).toHaveClass("text-rose-700")
    expect(container.querySelector(".lucide-command")).not.toBeNull()
  })

  it("renders an expert with the SAME command glyph + color (unified, no star)", () => {
    const { container } = render(
      <ReferenceBadge
        data={ref({
          refType: "skill",
          id: "reviewer",
          label: "Reviewer",
          meta: { scope: "expert", invocationPrefix: "/" },
        })}
      />
    )
    const badge = badgeOf(container)
    expect(badge).toHaveAttribute("data-ref-type", "skill")
    // Experts are no longer distinguished — same rose color, command glyph.
    expect(badge).toHaveClass("text-rose-700")
    expect(container.querySelector(".lucide-command")).not.toBeNull()
    expect(container.querySelector(".lucide-sparkles")).toBeNull()
  })
})
