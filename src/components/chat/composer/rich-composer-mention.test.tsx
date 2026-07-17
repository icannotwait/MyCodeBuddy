import { act, render, screen, waitFor } from "@testing-library/react"
import type { JSONContent } from "@tiptap/core"
import { createRef } from "react"
import { describe, expect, it, vi } from "vitest"

import { RichComposer, type RichComposerHandle } from "./rich-composer"
import type {
  ReferenceSearchController,
  ReferenceSearchSnapshot,
} from "./reference-search-controller"
import type { ReferenceAttrs } from "./types"
import { FakeReferenceSearchController } from "./suggestion/fake-reference-search-controller"

function fileItem(): {
  reference: ReferenceAttrs & { uri: string }
  detail: string
  selectable: boolean
  freshness: "fresh"
  sourceOrdinal: number
  regexRank: null
} {
  return {
    reference: {
      refType: "file",
      id: "src/app.ts",
      label: "app.ts",
      uri: "file:///repo/src/app.ts",
      meta: { fileKind: "file" },
    },
    // Keep detail distinct from the label for unambiguous queries.
    detail: "src/path/app.ts",
    selectable: true,
    freshness: "fresh",
    sourceOrdinal: 0,
    regexRank: null,
  }
}

function snapshotWithFile(query = "app"): ReferenceSearchSnapshot {
  return {
    query,
    generation: 1,
    patternError: false,
    groups: {
      agent: {
        kind: "agent",
        label: "Agents",
        items: [],
        loading: false,
        truncated: false,
        error: null,
      },
      file: {
        kind: "file",
        label: "Files",
        items: [fileItem()],
        loading: false,
        truncated: false,
        error: null,
      },
      session: {
        kind: "session",
        label: "Sessions",
        items: [],
        loading: false,
        truncated: false,
        error: null,
      },
      commit: {
        kind: "commit",
        label: "Commits",
        items: [],
        loading: false,
        truncated: false,
        error: null,
      },
    },
  }
}

function makeController(
  snapshot = snapshotWithFile()
): FakeReferenceSearchController {
  const controller = new FakeReferenceSearchController(snapshot)
  // Keep publishing the file row for any query so the integration stays simple.
  const originalSetQuery = controller.setQuery
  controller.setQuery = (query: string) => {
    controller.publish(snapshotWithFile(query))
    originalSetQuery(query)
  }
  return controller
}

function findReference(doc: JSONContent): JSONContent | undefined {
  if (doc.type === "reference") return doc
  for (const child of doc.content ?? []) {
    const found = findReference(child)
    if (found) return found
  }
  return undefined
}

async function mount(
  onSubmit?: () => void,
  controller: FakeReferenceSearchController | null = makeController()
) {
  const ref = createRef<RichComposerHandle>()
  const view = render(
    <RichComposer
      ref={ref}
      referenceController={controller as ReferenceSearchController | null}
      onSubmit={onSubmit}
    />
  )
  await waitFor(() => expect(ref.current?.getEditor()).not.toBeNull(), {
    timeout: 5000,
  })
  const editor = ref.current?.getEditor()
  if (!editor) throw new Error("editor not mounted")
  return { ref, editor, controller, rerender: view.rerender, onSubmit }
}

describe("RichComposer @ mention integration", () => {
  it("opens the panel on @ and inserts the chosen reference", async () => {
    const { editor } = await mount()
    act(() => {
      editor.commands.insertContent("@app")
    })
    const row = await screen.findByText("app.ts", {}, { timeout: 5000 })
    act(() => {
      row.dispatchEvent(
        new MouseEvent("mousedown", { bubbles: true, cancelable: true })
      )
    })
    await waitFor(() => {
      const node = findReference(editor.getJSON())
      expect(node?.attrs).toMatchObject({ refType: "file", id: "src/app.ts" })
    })
    expect(editor.getText()).not.toContain("@app")
    const dom = editor.view.dom as HTMLElement
    await waitFor(() => {
      expect(dom.hasAttribute("aria-controls")).toBe(false)
      expect(dom.hasAttribute("aria-activedescendant")).toBe(false)
      expect(dom.hasAttribute("aria-autocomplete")).toBe(false)
    })
  })

  it("wires the editor's combobox ARIA while the panel is open, clears it on Escape", async () => {
    const { editor } = await mount()
    const dom = editor.view.dom as HTMLElement
    expect(dom.getAttribute("role")).toBe("textbox")
    expect(dom.hasAttribute("aria-controls")).toBe(false)
    act(() => {
      editor.commands.insertContent("@app")
    })
    await screen.findByText("app.ts", {}, { timeout: 5000 })
    await waitFor(() => {
      expect(dom.getAttribute("aria-controls")).toBe("mention-listbox")
      expect(dom.getAttribute("aria-autocomplete")).toBe("list")
      expect(dom.getAttribute("aria-activedescendant")).toBe(
        "mention-option-file-0"
      )
    })
    act(() => {
      dom.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Escape",
          bubbles: true,
          cancelable: true,
        })
      )
    })
    await waitFor(() => {
      expect(dom.hasAttribute("aria-controls")).toBe(false)
      expect(dom.hasAttribute("aria-autocomplete")).toBe(false)
      expect(dom.hasAttribute("aria-activedescendant")).toBe(false)
    })
  })

  it("does not submit on Enter while the panel is open", async () => {
    const onSubmit = vi.fn()
    const { editor } = await mount(onSubmit)
    act(() => {
      editor.commands.insertContent("@app")
    })
    await screen.findByText("app.ts", {}, { timeout: 5000 })
    act(() => {
      ;(editor.view.dom as HTMLElement).dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          bubbles: true,
          cancelable: true,
        })
      )
    })
    expect(onSubmit).not.toHaveBeenCalled()
  })

  it("dismisses the panel on Escape", async () => {
    const { editor } = await mount()
    act(() => {
      editor.commands.insertContent("@app")
    })
    await screen.findByText("app.ts", {}, { timeout: 5000 })
    act(() => {
      ;(editor.view.dom as HTMLElement).dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Escape",
          bubbles: true,
          cancelable: true,
        })
      )
    })
    await waitFor(() => expect(screen.queryByText("app.ts")).toBeNull())
  })

  it("dismisses the panel and restores submit when referenceController is removed mid-open", async () => {
    const onSubmit = vi.fn()
    const controller = makeController()
    const ref = createRef<RichComposerHandle>()
    const { rerender } = render(
      <RichComposer
        ref={ref}
        referenceController={controller as ReferenceSearchController}
        onSubmit={onSubmit}
      />
    )
    await waitFor(() => expect(ref.current?.getEditor()).not.toBeNull(), {
      timeout: 5000,
    })
    const editor = ref.current?.getEditor()
    if (!editor) throw new Error("editor not mounted")
    act(() => {
      editor.commands.insertContent("@app")
    })
    await screen.findByText("app.ts", {}, { timeout: 5000 })

    rerender(<RichComposer ref={ref} onSubmit={onSubmit} />)
    await waitFor(() =>
      expect(screen.queryByTestId("mention-popup")).toBeNull()
    )
    const dom = editor.view.dom as HTMLElement
    expect(dom.hasAttribute("aria-controls")).toBe(false)
    expect(dom.hasAttribute("aria-activedescendant")).toBe(false)
    expect(dom.hasAttribute("aria-autocomplete")).toBe(false)

    act(() => {
      ;(editor.view.dom as HTMLElement).dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          bubbles: true,
          cancelable: true,
        })
      )
    })
    expect(onSubmit).toHaveBeenCalled()
  })

  it("does not open a panel when referenceController is not provided", async () => {
    const ref = createRef<RichComposerHandle>()
    render(<RichComposer ref={ref} />)
    await waitFor(() => expect(ref.current?.getEditor()).not.toBeNull(), {
      timeout: 5000,
    })
    const editor = ref.current?.getEditor()
    if (!editor) throw new Error("editor not mounted")
    act(() => {
      editor.commands.insertContent("@app")
    })
    await new Promise((resolve) => setTimeout(resolve, 250))
    expect(screen.queryByTestId("mention-popup")).toBeNull()
  })

  it("controller_becomes_available_after_editor_mount_without_rebuilding_the_editor", async () => {
    const ref = createRef<RichComposerHandle>()
    const { rerender } = render(
      <RichComposer ref={ref} referenceController={null} />
    )
    await waitFor(() => expect(ref.current?.getEditor()).not.toBeNull(), {
      timeout: 5000,
    })
    const editor = ref.current?.getEditor()
    if (!editor) throw new Error("editor not mounted")

    const controller = makeController()
    rerender(
      <RichComposer
        ref={ref}
        referenceController={controller as ReferenceSearchController}
      />
    )
    expect(ref.current?.getEditor()).toBe(editor)

    act(() => {
      editor.commands.insertContent("@")
    })
    expect(
      await screen.findByText("app.ts", {}, { timeout: 5000 })
    ).toBeTruthy()
    expect(ref.current?.getEditor()).toBe(editor)
  })

  it("replacing_or_disabling_controller_has_one_idempotent_close_effect", async () => {
    const controllerA = makeController()
    const controllerB = makeController()
    const ref = createRef<RichComposerHandle>()
    const { rerender } = render(
      <RichComposer
        ref={ref}
        referenceController={controllerA as ReferenceSearchController}
      />
    )
    await waitFor(() => expect(ref.current?.getEditor()).not.toBeNull(), {
      timeout: 5000,
    })
    const editor = ref.current?.getEditor()
    if (!editor) throw new Error("editor not mounted")

    act(() => {
      editor.commands.insertContent("@app")
    })
    await screen.findByRole("option", { name: /^app\.ts/ }, { timeout: 5000 })
    expect(controllerA.closeCallCount()).toBe(0)

    await act(async () => {
      rerender(
        <RichComposer
          ref={ref}
          referenceController={controllerB as ReferenceSearchController}
        />
      )
      await Promise.resolve()
    })
    expect(screen.queryByTestId("mention-popup")).toBeNull()
    // One active→inactive close for A (prop change + onExit both call close;
    // second is a no-op).
    expect(controllerA.closeCallCount()).toBe(1)

    act(() => {
      editor.commands.clearContent()
    })
    act(() => {
      editor.commands.insertContent("@app")
    })
    await screen.findByRole("option", { name: /^app\.ts/ }, { timeout: 5000 })

    await act(async () => {
      rerender(<RichComposer ref={ref} referenceController={null} />)
      await Promise.resolve()
    })
    expect(screen.queryByTestId("mention-popup")).toBeNull()
    expect(controllerB.closeCallCount()).toBe(1)
    // No stale insertion after teardown.
    expect(findReference(editor.getJSON())).toBeUndefined()
  }, 15000)
})
