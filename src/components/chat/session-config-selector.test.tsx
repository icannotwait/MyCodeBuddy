import { render, screen, cleanup, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it, vi } from "vitest"

import { InlineSessionConfigSelector } from "./session-config-selector"
import { deriveModelGroups } from "@/lib/model-config-groups"
import type { SessionConfigOptionInfo } from "@/lib/types"

function modelOption(
  options: { value: string; name: string; description?: string | null }[],
  current = options[0]?.value ?? ""
): SessionConfigOptionInfo {
  return {
    id: "model",
    name: "Model",
    description: null,
    category: null,
    kind: {
      type: "select",
      current_value: current,
      options: options.map((o) => ({ description: null, ...o })),
      groups: [],
    },
  }
}

describe("InlineSessionConfigSelector — model grouping", () => {
  afterEach(() => cleanup())

  it("keeps the current label visible when disabled and cannot fire onSelect", async () => {
    const user = userEvent.setup()
    const option = modelOption(
      [
        { value: "a", name: "Alpha" },
        { value: "b", name: "Beta" },
      ],
      "a"
    )
    const onSelect = vi.fn()
    render(
      <InlineSessionConfigSelector
        option={option}
        onSelect={onSelect}
        disabled
      />
    )
    const trigger = screen.getByRole("button", { name: /Alpha/ })
    expect(trigger).toBeDisabled()
    expect(trigger).toHaveTextContent("Alpha")
    await user.click(trigger)
    expect(onSelect).not.toHaveBeenCalled()
    expect(screen.queryByRole("menuitemradio")).toBeNull()
  })

  it("renders provider headers and prefix-stripped labels for derived groups", async () => {
    const user = userEvent.setup()
    const option = modelOption(
      [
        { value: "anthropic/claude-opus", name: "anthropic/claude-opus" },
        { value: "openai/gpt-4o", name: "openai/gpt-4o" },
      ],
      "anthropic/claude-opus"
    )
    const onSelect = vi.fn()
    render(
      <InlineSessionConfigSelector
        option={option}
        derivedGroups={deriveModelGroups(option)}
        onSelect={onSelect}
      />
    )

    // The trigger shows the selected model with its `provider/` prefix
    // stripped (the provider is implied by its group) — not `anthropic/...`.
    const trigger = screen.getByRole("button", { name: /claude-opus/ })
    expect(trigger).not.toHaveTextContent("anthropic/")
    await user.click(trigger)

    // Provider namespaces become headers.
    expect(await screen.findByText("anthropic")).toBeInTheDocument()
    expect(screen.getByText("openai")).toBeInTheDocument()
    // In-group labels drop the redundant `provider/` prefix.
    const item = screen.getByRole("menuitemradio", { name: /claude-opus/ })
    expect(item).toBeInTheDocument()
  })

  it("headers with the human provider name and strips it from rows (value≠name)", async () => {
    const user = userEvent.setup()
    // Real OpenCode shape: ids are `opencode/…` but names repeat `OpenCode Zen/`.
    const option = modelOption(
      [
        { value: "opencode/big-pickle", name: "OpenCode Zen/Big Pickle" },
        { value: "opencode/claude-haiku", name: "OpenCode Zen/Claude Haiku" },
        { value: "anthropic/claude-opus", name: "anthropic/claude-opus" },
      ],
      "opencode/big-pickle"
    )
    const onSelect = vi.fn()
    render(
      <InlineSessionConfigSelector
        option={option}
        derivedGroups={deriveModelGroups(option)}
        onSelect={onSelect}
      />
    )
    // The trigger shows the stripped current label, not "OpenCode Zen/…".
    const trigger = screen.getByRole("button", { name: /Big Pickle/ })
    expect(trigger).not.toHaveTextContent("OpenCode Zen/")
    await user.click(trigger)

    // The header is the human provider name (not the `opencode` id).
    expect(await screen.findByText("OpenCode Zen")).toBeInTheDocument()
    // Rows drop the repeated prefix but commit the full id.
    const haiku = screen.getByRole("menuitemradio", { name: /Claude Haiku/ })
    expect(haiku).not.toHaveTextContent("OpenCode Zen/")
    await user.click(haiku)
    expect(onSelect).toHaveBeenCalledWith("model", "opencode/claude-haiku")
  })

  it("commits the full value (not the stripped label) on select", async () => {
    const user = userEvent.setup()
    const option = modelOption([
      { value: "anthropic/claude-opus", name: "anthropic/claude-opus" },
      { value: "openai/gpt-4o", name: "openai/gpt-4o" },
    ])
    const onSelect = vi.fn()
    render(
      <InlineSessionConfigSelector
        option={option}
        derivedGroups={deriveModelGroups(option)}
        onSelect={onSelect}
      />
    )
    await user.click(screen.getByRole("button", { name: /claude-opus/ }))
    await user.click(
      await screen.findByRole("menuitemradio", { name: /gpt-4o/ })
    )
    expect(onSelect).toHaveBeenCalledWith("model", "openai/gpt-4o")
  })

  it("renders the floating bucket with no header before provider groups", async () => {
    const user = userEvent.setup()
    const option = modelOption(
      [
        { value: "default", name: "Default" },
        { value: "anthropic/opus", name: "anthropic/opus" },
      ],
      "default"
    )
    render(
      <InlineSessionConfigSelector
        option={option}
        derivedGroups={deriveModelGroups(option)}
        onSelect={vi.fn()}
      />
    )
    await user.click(screen.getByRole("button", { name: /Default/ }))

    // The prefix-less "Default" option is present…
    expect(
      await screen.findByRole("menuitemradio", { name: /Default/ })
    ).toBeInTheDocument()
    // …with a provider header for the grouped one, but no "Default" header.
    expect(screen.getByText("anthropic")).toBeInTheDocument()
    // Inside the menu "Default" appears once (the option), not also as a group
    // label (the floating bucket is headerless). The trigger's copy of the
    // current label lives outside the menu, so scope the count to the menu.
    const menu = screen.getByRole("menu")
    expect(within(menu).getAllByText(/^Default$/)).toHaveLength(1)
  })

  it("falls back to a flat list when no grouping applies", async () => {
    const user = userEvent.setup()
    const option = modelOption(
      [
        { value: "opus", name: "Opus" },
        { value: "haiku", name: "Haiku" },
      ],
      "opus"
    )
    render(
      <InlineSessionConfigSelector
        option={option}
        derivedGroups={deriveModelGroups(option)}
        onSelect={vi.fn()}
      />
    )
    await user.click(screen.getByRole("button", { name: /Opus/ }))
    expect(
      await screen.findByRole("menuitemradio", { name: /Haiku/ })
    ).toBeInTheDocument()
    // No provider headers for an ungroupable flat list.
    expect(screen.queryByText("anthropic")).toBeNull()
  })
})
