import { Editor } from "@tiptap/core"
import { afterEach, describe, expect, it } from "vitest"
import { SuggestionPluginKey } from "@tiptap/suggestion"

import { buildComposerExtensions } from "../editor-config"
import type {
  MentionController,
  MentionRenderState,
} from "./mention-suggestion"

class MentionCompositionFixture {
  readonly editor: Editor
  readonly element: HTMLDivElement
  private searches = 0
  private inserts = 0
  private submits = 0
  private query: string | null = null
  private open = false

  private constructor(initialText: string) {
    this.element = document.createElement("div")
    document.body.appendChild(this.element)

    const controller: MentionController = {
      onStart: (state) => this.noteSearch(state),
      onUpdate: (state) => this.noteSearch(state),
      onExit: () => {
        this.open = false
        this.query = null
      },
      onKeyDown: (event) => {
        if (event.key === "Enter") {
          this.inserts += 1
          return true
        }
        return false
      },
    }

    this.editor = new Editor({
      element: this.element,
      extensions: buildComposerExtensions({ mentionController: controller }),
    })

    this.editor.commands.setContent(initialText)
    this.editor.commands.focus("end")
    // Close any setContent-triggered session so compositionend rematch is the
    // only counted open. The suggestion plugin's view.update is async
    // (`await items()`), so callers must flush before reading counters.
    this.editor.view.dispatch(
      this.editor.view.state.tr.setMeta(SuggestionPluginKey, { exit: true })
    )
  }

  static async create(initialText: string): Promise<MentionCompositionFixture> {
    const fixture = new MentionCompositionFixture(initialText)
    // Drain the async suggestion view.update chain from setContent + exit.
    await fixture.flushMicrotask()
    await fixture.flushMicrotask()
    fixture.resetCounters()
    return fixture
  }

  private noteSearch(state: MentionRenderState): void {
    this.open = true
    this.query = state.query
    this.searches += 1
  }

  private resetCounters(): void {
    this.searches = 0
    this.inserts = 0
    this.submits = 0
    this.query = null
    this.open = false
  }

  startComposition(): void {
    this.editor.view.dom.dispatchEvent(
      new CompositionEvent("compositionstart", {
        bubbles: true,
        cancelable: true,
      })
    )
  }

  endComposition(): void {
    this.editor.view.dom.dispatchEvent(
      new CompositionEvent("compositionend", {
        bubbles: true,
        cancelable: true,
      })
    )
  }

  dispatchKey(init: {
    key: string
    isComposing?: boolean
    keyCode?: number
  }): void {
    const event = new KeyboardEvent("keydown", {
      key: init.key,
      bubbles: true,
      cancelable: true,
    })
    Object.defineProperty(event, "isComposing", {
      value: init.isComposing ?? false,
    })
    if (init.keyCode != null) {
      Object.defineProperty(event, "keyCode", { value: init.keyCode })
    }
    // Track submit attempts that reach the editor outside the mention plugin.
    if (
      init.key === "Enter" &&
      !init.isComposing &&
      init.keyCode !== 229 &&
      !this.open
    ) {
      this.submits += 1
    }
    this.editor.view.dom.dispatchEvent(event)
  }

  dispatchEquivalentProseMirrorTransaction(): void {
    this.editor.view.dispatch(
      this.editor.view.state.tr.setMeta(
        "codegMentionCompositionRematch",
        "equivalent"
      )
    )
  }

  async flushMicrotask(): Promise<void> {
    await Promise.resolve()
    await Promise.resolve()
  }

  searchCount(): number {
    return this.searches
  }

  insertCount(): number {
    return this.inserts
  }

  submitCount(): number {
    return this.submits
  }

  currentQuery(): string | null {
    return this.query
  }

  destroy(): void {
    if (!this.editor.isDestroyed) {
      this.editor.destroy()
    }
    this.element.remove()
  }
}

async function mentionCompositionFixture(
  initialText: string
): Promise<MentionCompositionFixture> {
  return MentionCompositionFixture.create(initialText)
}

const fixtures: MentionCompositionFixture[] = []

afterEach(() => {
  while (fixtures.length > 0) {
    fixtures.pop()?.destroy()
  }
})

describe("MentionSuggestion IME composition rematch", () => {
  it.each(["中文", "english"])(
    "rematches @%s exactly once after compositionend",
    async (text) => {
      const fixture = await mentionCompositionFixture(`@${text}`)
      fixtures.push(fixture)
      fixture.startComposition()
      fixture.dispatchKey({ key: "Enter", isComposing: true, keyCode: 229 })
      expect(fixture.searchCount()).toBe(0)
      fixture.endComposition()
      await fixture.flushMicrotask()
      // Async suggestion view.update needs another tick after the rematch tr.
      await fixture.flushMicrotask()
      expect(fixture.currentQuery()).toBe(text)
      expect(fixture.searchCount()).toBe(1)
      fixture.dispatchEquivalentProseMirrorTransaction()
      await fixture.flushMicrotask()
      expect(fixture.searchCount()).toBe(1)
      expect(fixture.insertCount()).toBe(0)
      expect(fixture.submitCount()).toBe(0)
    }
  )

  it("unmount_after_compositionend_before_microtask_is_a_noop", async () => {
    const fixture = await mentionCompositionFixture("@pending")
    // Intentionally not tracked in fixtures — destroyed mid-flight.
    fixture.startComposition()
    fixture.endComposition()
    expect(() => fixture.destroy()).not.toThrow()
    await fixture.flushMicrotask()
    await fixture.flushMicrotask()
    expect(fixture.searchCount()).toBe(0)
    expect(fixture.insertCount()).toBe(0)
    expect(fixture.submitCount()).toBe(0)
  })
})
