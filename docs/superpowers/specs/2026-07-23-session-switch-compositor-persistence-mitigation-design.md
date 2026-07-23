# Session Switch Compositor Persistence Mitigation Design

Date: 2026-07-23

Status: Design approved in conversation; written-spec review pending

## Summary

Prevent stale status controls from remaining visible after a main-window
conversation switch by removing inactive, non-tiled conversation panels from
layout and paint with the native HTML `hidden` attribute. The React subtree
remains mounted under its stable tab ID, so ACP connections, background turns,
drafts, component state, and scroll state remain alive.

This is a narrow rendering mitigation. It does not change tab identity,
connection ownership, conversation loading, runtime stores, preview-tab
replacement, or tiled mode.

## Evidence

The earlier diagnostics design intentionally deferred a fix until the visible
failure was identified. The supplied before/after screenshots now provide that
evidence:

1. The target conversation body has already committed while isolated status
   pills from the previous conversation remain at old screen coordinates.
2. Pixel template matching a completed-status pill from the old screenshot
   against the switched screenshot produced matches above 0.96 in positions
   where the target conversation has no status component.
3. All affected labels (completed, active, and running) resolve to the in-tree
   `StatusBadge`; the component does not use a portal.
4. Inactive tabs remain mounted and currently use only
   `visibility: hidden` through the Tailwind `invisible` class.
5. A Chromium layer-tree probe confirms that status spinners create
   accelerated transform layers and the conversation overlays create backdrop
   filter layers. The installed embedded runtime is WebView2 150.0.4078.83.

The application trigger is therefore the combination of a painted keep-alive
tree and ancestor visibility switching. The embedded compositor can retain old
raster damage after the active surface changes. Async detail loading, selector
rebinding, React key reuse, and portal ownership do not explain the screenshot.

## Goals

1. Ensure an inactive non-tiled conversation contributes no layout, paint, or
   compositor output.
2. Keep every conversation React subtree mounted under the same `tab.id`.
3. Preserve ACP connections and background execution while a tab is inactive.
4. Preserve drafts, local component state, and scroll position across switches.
5. Allow visual-only animations to pause while a panel is not displayed.
6. Leave tiled mode behavior unchanged.

## Non-Goals

- Unmounting inactive conversation surfaces.
- Disconnecting or pausing background agent work.
- Adding skeletons, detail prefetch, selector identity gates, or cache changes.
- Forcing repaint through layout reads, transform toggles, timers, or synthetic
  resize events.
- Changing the shared `Badge`, spinner, backdrop-filter, or virtualization
  implementation globally.
- Claiming that the application mitigation fixes WebView2 itself.

## Alternatives Considered

### Native `hidden` Attribute on the Keep-Alive Wrapper

Selected. Apply `hidden={!canTile && !active}` to each tab wrapper while still
constructing and rendering its `ConversationTabView`. Native `hidden` maps to
`display: none`, which removes the subtree from layout and paint without
unmounting React or running effect cleanup.

This is the smallest change that directly removes the stale compositor input.
It also naturally suspends visual animation work while the panel is absent.

### `content-visibility: hidden` or Paint Containment

Rejected for the first fix. These properties can reduce work and create a
stronger paint boundary, but they continue to exercise compositor visibility
paths and are not as strong a teardown signal as `display: none`. They add
browser-specific behavior without evidence that they clear the observed tiles.

### Unmount Only the Message Visual Tree

Rejected. Retaining connection providers while conditionally unmounting the
message list would isolate paint, but it expands the change into local state,
scroll restoration, overlay state, and live-footer handoff. The full tab must
remain mounted by product requirement, and native `hidden` provides that
without a new ownership boundary.

## Design

### Tab Wrapper

`ConversationDetailPanel` continues to map every tab with `key={tab.id}` and
always creates its `ConversationTabView`. The wrapper receives:

```tsx
hidden={!canTile && !active}
```

Its class selection becomes:

- tiled: the existing relative flex panel classes;
- active non-tiled: `h-full`;
- inactive non-tiled: no absolute/invisible fallback class, because `hidden`
  owns layout and paint suppression.

The `isActive` prop continues to drive connection activity bookkeeping exactly
as before. The hidden attribute affects presentation only.

### Background Behavior

React does not unmount children when an ancestor gains `hidden`. Existing
effects, stores, subscriptions, and ACP connections therefore remain alive.
Background status changes continue to update state. CSS animations under the
non-rendered subtree may pause or restart; only their latest state matters when
the tab is shown again.

### Reactivation and Virtualization

Removing `hidden` restores the same DOM and scroll element. Virtua and the
scroll shell already observe size changes, so the first implementation relies
on their normal ResizeObserver path. It does not add a forced reflow or a
global resize event.

If runtime verification exposes a zero-sized or stale virtual viewport after
reactivation, that is a separate observed failure and will receive its own
minimal measurement fix. It is not preemptively bundled into this mitigation.

### Tiled Mode

When `canTile` is true, `hidden` is false for every tab. Existing horizontal
layout, active-tile selection, borders, and pointer activation remain unchanged.

## Testing Strategy

Follow red-green order:

1. Add a focused layout contract test that fails while the panel still uses
   `absolute inset-0 invisible pointer-events-none`.
2. Assert the wrapper uses `hidden={!canTile && !active}`.
3. Assert the stable `key={tab.id}` and unconditional `ConversationTabView`
   construction remain present, proving the fix is keep-alive rather than
   conditional unmounting.
4. Assert tiled panels retain their existing visible layout branch.
5. Run the focused layout test, affected conversation/message tests, the full
   Vitest suite, ESLint, and the static export build.
6. In the desktop WebView2 runtime, switch repeatedly between a delegation-
   heavy tab and a text-only tab. Verify no previous status pill remains and
   that returning to the first tab preserves its scroll position, draft, and
   background progress.

DOM unit tests cannot reproduce a native compositor defect; they protect the
rendering contract that avoids it. Desktop runtime verification remains part of
acceptance.

## Risks and Mitigations

### Virtualizer Observes a Zero Viewport

`display: none` gives descendants zero layout size while inactive. Existing
ResizeObserver behavior should remeasure on activation. Runtime verification
must cover a long, scrolled conversation before accepting the change; no
speculative resize hack is added.

### Scroll Position Changes

The same scroll DOM node remains mounted, so its `scrollTop` should persist.
The manual acceptance path records the position before switching and confirms
it after returning.

### Background Work Accidentally Stops

The fix must not conditionally render `ConversationTabView` and must not change
connection lifecycle inputs. Tests retain the stable wrapper identity contract,
and runtime verification confirms a hidden agent turn continues progressing.

### Tiled Layout Regression

The hidden predicate explicitly excludes tile mode. Existing tile classes and
activation handling remain byte-for-byte unchanged where practical.

## Acceptance Criteria

1. After switching conversations, no status pill or other control from the
   previous tab remains visible in the target tab.
2. Inactive non-tiled wrappers use `display: none` through the native `hidden`
   attribute, not `visibility: hidden`.
3. Inactive conversation React subtrees remain mounted under stable tab IDs.
4. Background ACP work and state updates continue while a tab is hidden.
5. Returning to a hidden tab preserves its draft and scroll position and shows
   current background status.
6. Tiled mode continues to display all conversation panels.
7. Focused tests, full frontend tests, ESLint, and the static export build pass.
8. A desktop WebView2 switch verifies the visual symptom is no longer present.
