import { Extension } from "@tiptap/core"
import { Plugin } from "@tiptap/pm/state"
import Suggestion, {
  findSuggestionMatch,
  SuggestionPluginKey,
  type SuggestionProps,
} from "@tiptap/suggestion"

/** Live render state the plugin pushes to React while the `@` panel is open. */
export interface MentionRenderState {
  query: string
  /** Document range covering `@` + query, replaced when a row is chosen. */
  range: { from: number; to: number }
  /**
   * Live caret-rect getter (viewport coords), or null if unknown. Call it at
   * position time — not once at trigger time — so the popup re-anchors to the
   * current caret after a window resize, editor scroll, or page scroll while it
   * is open.
   */
  getClientRect: (() => DOMRect | null) | null
}

/**
 * Callbacks the React layer supplies so the suggestion plugin can drive a React
 * popup that lives in the editor's component tree (where data hooks work). The
 * plugin owns trigger detection; React owns data + rendering + insertion.
 */
export interface MentionController {
  onStart: (state: MentionRenderState) => void
  onUpdate: (state: MentionRenderState) => void
  onExit: () => void
  /** Forwarded keydown; return true if the popup consumed it. */
  onKeyDown: (event: KeyboardEvent) => boolean
}

export interface MentionSuggestionOptions {
  controller: MentionController
}

const NOOP_CONTROLLER: MentionController = {
  onStart: () => {},
  onUpdate: () => {},
  onExit: () => {},
  onKeyDown: () => false,
}

const COMPOSITION_REMATCH_META = "codegMentionCompositionRematch"

function toRenderState(props: SuggestionProps): MentionRenderState {
  return {
    query: props.query,
    range: props.range,
    // Keep the getter itself (not a snapshot) so reposition reads live coords.
    getClientRect: props.clientRect ?? null,
  }
}

/**
 * Tiptap extension wiring `@tiptap/suggestion` (trigger `@`) to a
 * {@link MentionController}. Data fetching, rendering and insertion are handled
 * by the controller's React popup, so the plugin's own `items`/`command` are
 * intentionally inert.
 *
 * A companion ProseMirror plugin rematches once after IME `compositionend` so
 * CJK composition can close the panel without permanently suppressing `@`.
 */
export const MentionSuggestion = Extension.create<MentionSuggestionOptions>({
  name: "mentionSuggestion",

  addOptions() {
    return { controller: NOOP_CONTROLLER }
  },

  addProseMirrorPlugins() {
    const editor = this.editor
    const controller = this.options.controller
    const mentionMatchOptions = {
      char: "@",
      allowSpaces: false,
      allowToIncludeChar: false,
      allowedPrefixes: [" "] as string[],
      startOfLine: false,
    }
    // Editor-instance-local composition bookkeeping (closed over per
    // addProseMirrorPlugins invocation).
    let compositionSequence = 0
    // True after compositionstart closed the React popup; compositionend must
    // force a clean onStart even when the suggestion plugin stayed active.
    let needsCompositionRematch = false

    const compositionRematch = new Plugin({
      props: {
        handleDOMEvents: {
          compositionstart: () => {
            compositionSequence += 1
            needsCompositionRematch = true
            // Close the React popup and cancel pointer/validation without
            // exitSuggestion (which would set dismissedRange and block reopen).
            controller.onExit()
            return false
          },
          compositionend: (view) => {
            const sequence = ++compositionSequence
            queueMicrotask(() => {
              if (
                sequence !== compositionSequence ||
                view.isDestroyed ||
                view.composing
              ) {
                return
              }
              const match = findSuggestionMatch({
                ...mentionMatchOptions,
                $position: view.state.selection.$from,
              })
              const current = SuggestionPluginKey.getState(view.state) as
                | {
                    active?: boolean
                    query?: string | null
                    range?: { from: number; to: number }
                  }
                | undefined
              const sameRange =
                current?.range?.from === match?.range.from &&
                current?.range?.to === match?.range.to
              const alreadyOpen =
                Boolean(current?.active) &&
                current?.query === match?.query &&
                sameRange

              if (!match) {
                needsCompositionRematch = false
                return
              }
              // Idempotent: if the plugin already shows this match and we did
              // not close for composition, skip.
              if (alreadyOpen && !needsCompositionRematch) {
                return
              }
              // compositionstart closed React while the plugin stayed active —
              // exit first so the following rematch can fire a clean onStart.
              // shouldResetDismissed clears the dismissedRange exit sets.
              if (alreadyOpen && needsCompositionRematch) {
                view.dispatch(
                  view.state.tr.setMeta(SuggestionPluginKey, { exit: true })
                )
              }
              needsCompositionRematch = false
              if (view.isDestroyed) return
              view.dispatch(
                view.state.tr.setMeta(COMPOSITION_REMATCH_META, sequence)
              )
            })
            return false
          },
        },
      },
    })

    const mentionSuggestion = Suggestion({
      editor,
      ...mentionMatchOptions,
      items: () => [],
      command: () => {},
      // Clear exitSuggestion's dismissedRange when our rematch meta is present
      // so compositionend can reopen the same `@` range.
      shouldResetDismissed: ({ transaction }) =>
        transaction.getMeta(COMPOSITION_REMATCH_META) != null,
      allow: ({ state }) => {
        if (editor.view.composing) return false
        return !state.selection.$from.parent.type.spec.code
      },
      render: () => ({
        onStart: (props) => controller.onStart(toRenderState(props)),
        onUpdate: (props) => controller.onUpdate(toRenderState(props)),
        onExit: () => controller.onExit(),
        onKeyDown: ({ event }) => {
          if (
            event.isComposing ||
            event.keyCode === 229 ||
            editor.view.composing
          ) {
            return false
          }
          return controller.onKeyDown(event)
        },
      }),
    })
    return [mentionSuggestion, compositionRematch]
  },
})
