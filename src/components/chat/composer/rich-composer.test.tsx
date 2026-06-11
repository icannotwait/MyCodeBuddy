import { act, render, waitFor } from "@testing-library/react"
import { createRef } from "react"
import { describe, expect, it, vi } from "vitest"

import { RichComposer, type RichComposerHandle } from "./rich-composer"

/** Wait until the editor has mounted (immediatelyRender:false makes it async). */
async function mount(props: React.ComponentProps<typeof RichComposer> = {}) {
  const ref = createRef<RichComposerHandle>()
  const result = render(<RichComposer ref={ref} {...props} />)
  // Generous timeout: editor construction (ProseMirror + React node view) can
  // be slow under parallel worker CPU contention.
  await waitFor(() => expect(ref.current?.getEditor()).not.toBeNull(), {
    timeout: 5000,
  })
  return { ref, ...result }
}

describe("RichComposer", () => {
  it("mounts and reports an empty document via the handle", async () => {
    const { ref } = await mount()
    expect(ref.current?.isEmpty()).toBe(true)
    expect(ref.current?.getMarkdown()).toBe("")
  })

  it("paints the placeholder on the empty document", async () => {
    const { ref, container } = await mount({ placeholder: "Ask anything" })
    expect(ref.current).not.toBeNull()
    expect(
      container.querySelector('[data-placeholder="Ask anything"]')
    ).not.toBeNull()
  })

  it("exposes an accessible multiline textbox", async () => {
    const { container } = await mount({ ariaLabel: "Message" })
    const textbox = container.querySelector('[role="textbox"]')
    expect(textbox).not.toBeNull()
    expect(textbox).toHaveAttribute("aria-multiline", "true")
    expect(textbox).toHaveAttribute("aria-label", "Message")
  })

  it("round-trips markdown through the handle and notifies onChange", async () => {
    const onChange = vi.fn()
    const { ref } = await mount({ onChange })

    act(() => {
      ref.current?.setMarkdown("hello **world**")
    })

    expect(ref.current?.getMarkdown()).toContain("**world**")
    expect(ref.current?.isEmpty()).toBe(false)
    expect(onChange).toHaveBeenCalled()
    const lastCall = onChange.mock.calls[onChange.mock.calls.length - 1]
    expect(lastCall?.[0]).toContain("**world**")

    act(() => {
      ref.current?.clear()
    })
    expect(ref.current?.isEmpty()).toBe(true)
  })

  it("preserves CJK content through the handle", async () => {
    const { ref } = await mount()
    act(() => {
      ref.current?.setMarkdown("发送给智能体的消息")
    })
    expect(ref.current?.getMarkdown()).toContain("发送给智能体的消息")
  })

  it("initializes from defaultMarkdown without firing onChange", async () => {
    const onChange = vi.fn()
    const { ref } = await mount({
      defaultMarkdown: "# Heading",
      onChange,
    })
    expect(ref.current?.getMarkdown().trim()).toBe("# Heading")
    // onCreate sets content with emitUpdate:false → no spurious change events.
    expect(onChange).not.toHaveBeenCalled()
  })
})

function dispatchKey(
  ref: React.RefObject<RichComposerHandle | null>,
  init: KeyboardEventInit
) {
  const dom = ref.current?.getEditor()?.view.dom as HTMLElement
  act(() => {
    dom.dispatchEvent(
      new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init })
    )
  })
}

describe("RichComposer imperative inserts (Phase 3)", () => {
  it("inserts markdown at the cursor", async () => {
    const { ref } = await mount()
    act(() => ref.current?.insertMarkdownAtCursor("hello **world**"))
    expect(ref.current?.getMarkdown()).toContain("**world**")
  })

  it("inserts a reference badge and exposes it via getJSON", async () => {
    const { ref } = await mount()
    act(() =>
      ref.current?.insertReference({
        refType: "file",
        id: "a.ts",
        label: "a.ts",
        uri: "file:///a.ts",
        meta: null,
      })
    )
    expect(JSON.stringify(ref.current?.getJSON())).toContain(
      '"type":"reference"'
    )
  })

  it("hydrates the document from a Tiptap JSON doc via setDoc", async () => {
    const { ref } = await mount()
    act(() =>
      ref.current?.setDoc({
        type: "doc",
        content: [
          { type: "paragraph", content: [{ type: "text", text: "from json" }] },
        ],
      })
    )
    expect(ref.current?.getMarkdown()).toContain("from json")
    expect(ref.current?.isEmpty()).toBe(false)
  })

  it("preserves a reference badge through a getJSON → setDoc round-trip", async () => {
    const { ref } = await mount()
    act(() =>
      ref.current?.insertReference({
        refType: "file",
        id: "a.ts",
        label: "a.ts",
        uri: "file:///a.ts",
        meta: null,
      })
    )
    const doc = ref.current!.getJSON()
    act(() => ref.current?.clear())
    expect(ref.current?.isEmpty()).toBe(true)
    act(() => ref.current?.setDoc(doc))
    expect(JSON.stringify(ref.current?.getJSON())).toContain(
      '"type":"reference"'
    )
  })
})

describe("RichComposer configurable submit / newline (Phase 3)", () => {
  it("submits on a plain Enter by default", async () => {
    const onSubmit = vi.fn()
    const { ref } = await mount({ onSubmit })
    dispatchKey(ref, { key: "Enter" })
    expect(onSubmit).toHaveBeenCalledTimes(1)
  })

  it("treats Enter as a newline when submitShortcut is mod+enter", async () => {
    const onSubmit = vi.fn()
    const { ref } = await mount({ onSubmit, submitShortcut: "mod+enter" })
    dispatchKey(ref, { key: "Enter" })
    expect(onSubmit).not.toHaveBeenCalled()
    dispatchKey(ref, { key: "Enter", metaKey: true })
    expect(onSubmit).toHaveBeenCalledTimes(1)
  })

  it("inserts a hard break on Shift+Enter without submitting", async () => {
    const onSubmit = vi.fn()
    const { ref } = await mount({ onSubmit })
    act(() => ref.current?.focus())
    dispatchKey(ref, { key: "Enter", shiftKey: true })
    expect(onSubmit).not.toHaveBeenCalled()
    expect(JSON.stringify(ref.current?.getJSON())).toContain(
      '"type":"hardBreak"'
    )
  })

  it("does not submit while an external menu is open", async () => {
    const onSubmit = vi.fn()
    const { ref } = await mount({ onSubmit, isExternalMenuOpen: true })
    dispatchKey(ref, { key: "Enter" })
    expect(onSubmit).not.toHaveBeenCalled()
  })

  it("submits on a custom non-Enter binding (Tab)", async () => {
    const onSubmit = vi.fn()
    const { ref } = await mount({ onSubmit, submitShortcut: "tab" })
    dispatchKey(ref, { key: "Tab" })
    expect(onSubmit).toHaveBeenCalledTimes(1)
  })

  it("breaks on a custom newline binding (Shift+Tab) without submitting", async () => {
    const onSubmit = vi.fn()
    const { ref } = await mount({ onSubmit, newlineShortcut: "shift+tab" })
    act(() => ref.current?.focus())
    dispatchKey(ref, { key: "Tab", shiftKey: true })
    expect(onSubmit).not.toHaveBeenCalled()
    expect(JSON.stringify(ref.current?.getJSON())).toContain(
      '"type":"hardBreak"'
    )
  })

  it("does not swallow Enter when no onSubmit handler is provided", async () => {
    const { ref } = await mount()
    act(() => ref.current?.setMarkdown("hello"))
    act(() => ref.current?.focus())
    dispatchKey(ref, { key: "Enter" })
    // Enter fell through to the editor default (paragraph split), not swallowed.
    expect(ref.current?.getJSON().content?.length).toBeGreaterThanOrEqual(2)
  })
})
