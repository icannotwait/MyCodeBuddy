# Grok Native Ask User Question Design

## Problem

Grok's ACP agent exposes a native `ask_user_question` tool. The tool emits a
normal `tool_call` with this input shape and then completes the turn:

```json
{
  "questions": [
    {
      "question": "Which behavior do you prefer?",
      "options": [
        { "label": "A", "description": "..." },
        { "label": "B", "description": "..." }
      ]
    }
  ]
}
```

The tool metadata identifies the request as a Grok-native user prompt:

```json
{
  "x.ai/tool": {
    "name": "ask_user_question",
    "kind": "ask_user",
    "namespace": "grok_build"
  }
}
```

The message renderer normalizes this tool to `question` and displays the
read-only `AskQuestionResultCard`. Its badges summarize the question; they are
not controls. The existing legacy question detector only accepts a singular
top-level `question` string, so it deliberately ignores the `questions` array.
Consequently no interactive question UI is mounted.

Grok's persisted session proves the expected response protocol: the question
turn ends immediately, and a normal user prompt containing the answer starts
the next turn. This differs from codeg-mcp `ask_user_question`, which remains
blocked and must be resolved through the dedicated answer endpoint.

## Goals

- Render Grok-native multiple-choice questions as interactive controls.
- Require users to select options and then press Submit; selection alone must
  not send a prompt.
- Support single-select, multi-select, and free-text Other answers through the
  existing `AskQuestionCard` behavior.
- Send the submitted answer as the next normal user prompt so Grok can resume.
- Preserve the existing free-text legacy question flow.
- Preserve the codeg-mcp blocking question flow and its answer API unchanged.

## Non-Goals

- Making the read-only question summary inside the transcript clickable.
- Changing Grok's ACP protocol or persisted session format.
- Retrofitting historical question calls with inferred answers.
- Changing codeg-mcp question registration, persistence, or resolution.

## Approaches Considered

### Extend the answer-as-next-prompt question flow (selected)

Represent the legacy pending question as a discriminated union with free-text
and structured-choice variants. Detect Grok-native choice questions when their
turn completes, render the choice variant with `AskQuestionCard`, and translate
its submitted `QuestionAnswer` into a normal prompt.

This reuses the established question UI while keeping response routing aligned
with Grok's actual protocol.

### Add a third independent pending-question state

A dedicated `pendingGrokQuestion` would isolate the feature, but it would
duplicate reducer actions, snapshot/reset behavior, `ConversationShell` props,
and answer handlers. The protocol difference does not justify that extra state.

### Make the transcript result card interactive

This would put the controls where the current summary appears, but the message
renderer has no prompt-sending ownership. Threading mutable connection actions
through the historical rendering stack would couple transcript rendering to
live session state and complicate history/reconnect behavior.

## Design

### Native Question Identification

The turn-complete scan continues to walk tool calls from newest to oldest. A
tool call is treated as a Grok-native structured question only when both are
true:

1. Its normalized tool name is `question`.
2. Its `x.ai/tool` metadata identifies `name = ask_user_question` and
   `kind = ask_user`.

The metadata gate is required. Input shape alone is insufficient because
codeg-mcp also sends a top-level `questions` array but uses a different response
protocol.

For a matching call, `parseAskQuestionInput` parses the tolerant raw input.
Each parsed question is converted to the existing `QuestionSpec` shape with:

- a stable synthetic ID derived from the tool-call ID and question index;
- the original question and options;
- `multi_select` copied from `multiSelect` when present, otherwise `false`;
- the original header, or the question text as a tab-label fallback.

Malformed or empty structured input does not mount a choice card. The existing
singular-string legacy detector remains the fallback.

### Pending State

`PendingQuestion` becomes a discriminated union:

```ts
type PendingQuestion =
  | {
      kind: "free_text"
      tool_call_id: string
      question: string
    }
  | {
      kind: "choice"
      tool_call_id: string
      question: PendingQuestionState
    }
```

This state remains frontend-only and answer-as-next-prompt. It is not merged
with `pendingAskQuestion`, whose value comes from backend `question_request`
events and snapshots and resolves through `acpAnswerQuestion`.

Starting a new prompt clears the pending native/legacy question through the
existing prompt lifecycle. Session replacement and disconnect behavior remain
unchanged.

### Rendering

`QuestionDialog` retains the free-text textarea for `kind = free_text`. For
`kind = choice`, it renders `AskQuestionCard` in the composer area and adapts
the card's `onAnswer` callback to the existing `onAnswer(string)` callback.

The transcript's `AskQuestionResultCard` remains a read-only record. The live
interactive card is visually distinct because it displays radio buttons or
checkboxes, descriptions, Other input, and an explicit Submit button.

### Answer Formatting

Submitting a structured answer produces one normal user prompt:

- For one question, send the selected labels joined by `, `.
- For multiple questions, send a numbered, human-readable list containing each
  question and its selected labels.
- Include non-empty Other text exactly as entered.
- If Skip is used, send a localized instruction to continue using the agent's
  best judgment. Skipping must still start a new turn; merely clearing the card
  would leave the workflow idle.

The existing optimistic user turn and `lifecycleSend` path are reused. Clicking
or toggling an option only changes local card state. Only Submit or Skip calls
`onAnswer`.

## Error Handling

- Invalid metadata: do not classify the call as a native structured question.
- Invalid JSON or no usable questions: fall back to the singular legacy
  question detector; otherwise render no interactive card.
- No selected answer: the existing `AskQuestionCard` keeps Submit disabled.
- Prompt send failure: rely on the existing message send lifecycle and error
  surface; do not call the codeg-mcp answer endpoint as a fallback.
- Duplicate or late turn events: stable tool-derived question IDs and existing
  reducer replacement semantics prevent stale selections from carrying over.

## Testing

### Parsing and Classification

- A real Grok `ask_user_question` tool call with `x.ai/tool.kind = ask_user`
  becomes a structured pending question.
- Missing `multiSelect` defaults to single-select.
- Missing headers receive deterministic fallbacks.
- Malformed or empty inputs do not create an unusable card.
- A codeg-mcp `ask_user_question` payload is not classified as a Grok-native
  answer-as-next-prompt question.
- The existing singular legacy question still creates a free-text prompt.

### Interaction

- Clicking a single-select option updates selection but does not call
  `onAnswer`.
- Clicking Submit sends the selected label exactly once.
- Multi-select answers and Other text are formatted without losing labels.
- Submit remains disabled until every question has an answer.
- Skip sends the continue-with-best-judgment prompt.

### Regression

- Existing `AskQuestionCard` tests remain green.
- Existing codeg-mcp question tests continue to prove that its plural payload
  does not trigger the legacy free-text path.
- The focused frontend tests, full Vitest suite, ESLint, and static export build
  are run before completion.

## Acceptance Criteria

1. Replaying the recorded Grok question shape produces a full interactive card
   above the composer after the question turn completes.
2. Selecting an option does not send anything until Submit is pressed.
3. Submit creates the next normal user message and Grok continues processing.
4. The codeg-mcp blocking question flow still calls its dedicated answer API
   and never creates a duplicate native card.
5. Free-text legacy questions continue to work as before.
