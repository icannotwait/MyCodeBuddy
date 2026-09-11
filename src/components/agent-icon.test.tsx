import { render } from "@testing-library/react"
import { describe, expect, it } from "vitest"

import { ALL_AGENT_TYPES, AGENT_LABELS } from "@/lib/types"

import { AgentIcon } from "./agent-icon"

describe("AgentIcon", () => {
  it("renders a compiled SVG mark for every built-in (never the empty swatch)", () => {
    for (const agentType of ALL_AGENT_TYPES) {
      const { container } = render(
        <AgentIcon agentType={agentType} className="h-4 w-4" />
      )
      const svg = container.querySelector("svg")
      expect(svg, `${agentType} should ship a brand SVG`).not.toBeNull()
      expect(
        svg?.querySelector("title")?.textContent,
        `${agentType} title`
      ).toBe(AGENT_LABELS[agentType])
      // The fallback is a colored circle with no svg. A builtin that misses
      // COLOR_ICONS / MONO_ICONS lands there and "the icon disappeared".
      expect(container.querySelector(":scope > span.rounded-full")).toBeNull()
    }
  })

  it.each(["qoder", "antigravity"] as const)(
    "draws %s on the same 24-box as the other boxless marks",
    (agentType) => {
      // These two were copied from the ACP registry's 16×16 currentColor
      // glyphs. Every other compiled mark uses a 24-box (or larger), and
      // AgentIcon sizes the SVG to 100% of a 12–16px wrapper — a 16-box
      // ring/arch collapses to a hairline or nothing at that size.
      const { container } = render(
        <AgentIcon agentType={agentType} className="h-4 w-4" />
      )
      const svg = container.querySelector("svg")
      expect(svg).not.toBeNull()
      expect(svg?.getAttribute("viewBox")).toBe("0 0 24 24")
      const path = svg?.querySelector("path")
      expect(path?.getAttribute("d")?.length).toBeGreaterThan(20)
    }
  )
})
