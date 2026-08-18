# Document Translation Inline Placeholder Reordering Design

## Status

Direction approved in conversation on 2026-08-18. This document is the
implementation specification.

No implementation plan has been approved yet. The implementation plan must
preserve the fail-closed invariants in this document.

## Executive Decision

Codeg will replace document translation's single global placeholder-order rule
with type-aware integrity validation:

- fenced-code placeholders (`CGCODE`) must remain in source order;
- inline-code placeholders (`CGINLINE`) may be reordered within the same
  fenced-code region; and
- every placeholder for the current request must remain byte-for-byte intact
  and occur exactly once.

A fenced-code region is the prose before the first `CGCODE`, between two
adjacent `CGCODE` placeholders, or after the last `CGCODE`. An inline
placeholder may change position inside its original region but may not cross a
fenced-code placeholder.

Restoration remains identity-based and fail-closed. Missing, duplicated,
unknown, malformed, altered, cross-region, or reordered fenced-code
placeholders reject the entire translation result.

## Problem

Markdown translation currently protects fenced and inline code by replacing
each code span with a nonce-scoped placeholder. After translation,
`restore_markdown` extracts placeholders from model output and compares their
complete sequence with the source sequence. Any order change fails.

That rule rejects valid target-language grammar. The observed regression was:

```text
Source:     It queries `id, data` from `blobs`.
Translated: It queries-from `blobs` the fields `id, data`.
```

The selected Cursor model returned all 88 placeholders exactly once and did
not alter any token. It only swapped the two adjacent inline placeholders that
represented `id, data` and `blobs`. The translation was semantically correct,
but the global ordered comparison returned placeholder-integrity failure.

This is more likely in technical documents because inline code commonly acts
as a noun phrase, object, table name, field list, command, path, or identifier.
Those phrases legitimately move when word order changes between languages.

## Goals

- Accept grammatically necessary reordering of intact inline-code
  placeholders.
- Preserve strict ordering for fenced code blocks.
- Prevent inline placeholders from moving across fenced code blocks.
- Continue rejecting missing, duplicated, unknown, malformed, or altered
  current-request placeholders.
- Restore each placeholder to its exact original Markdown bytes.
- Keep the existing command result, application error, and localized UI error
  contracts unchanged.
- Keep validation linear in the translated output plus placeholder count.
- Add diagnostics without logging source text, translated text, original code,
  placeholder tokens, or the request nonce.

## Non-Goals

- Parsing or semantically validating the translated prose.
- Allowing fenced code blocks to reorder.
- Allowing an inline placeholder to cross a fenced code block.
- Introducing Markdown AST translation, per-node model calls, or structured
  model output.
- Adding automatic retries or a second model call.
- Changing placeholder spelling or replacing the nonce scheme.
- Changing plain-text translation behavior.
- Changing streaming preview behavior.
- Detecting arbitrary placeholder-looking text that belongs to another nonce.

## Existing Behavior

`protect_markdown` currently performs these operations:

1. Replace each complete or EOF-terminated fenced code block with a unique
   `CGCODE` placeholder.
2. Replace supported single-backtick inline code spans outside fenced blocks
   with unique `CGINLINE` placeholders.
3. Sort the private placeholder table into source-document order.
4. Send the protected body to the translation agent.
5. Extract current-nonce placeholders from the returned text.
6. Require the extracted vector to equal the expected vector exactly.
7. Replace each validated token with its original bytes.

Steps 1 through 4 remain unchanged. Steps 5 and 6 become type-aware. Step 7
continues to restore by unique token identity rather than by ordinal position.

## Integrity Model

### Placeholder identity

Each protected placeholder has these logical attributes:

```text
kind         CGCODE or CGINLINE
nonce        random request nonce
index        per-kind source index
token        complete rendered placeholder
original     exact Markdown bytes to restore
fence_region number of CGCODE placeholders preceding it in source
```

`fence_region` is relevant only to `CGINLINE`. It can be computed while
walking the existing source-ordered placeholder table and does not need to be
encoded into the token.

The expected token string remains the primary identity. Kind and index parsed
from model output must agree with an expected token for the current nonce.

### Fenced-code regions

For `N` fenced-code placeholders, the document has `N + 1` regions:

```text
region 0: document start through CGCODE 0
region 1: after CGCODE 0 through CGCODE 1
...
region N: after CGCODE N-1 through document end
```

The boundary placeholder itself is not part of either prose region. An inline
placeholder's region is the number of valid `CGCODE` placeholders that precede
it.

If a document has no fenced code, all inline placeholders are in region 0 and
may reorder relative to one another. Paragraph, list-item, table-cell, and
heading boundaries are intentionally not integrity boundaries in this version.
Models may legitimately reflow those structures during translation, and the
current protection layer does not own a Markdown AST that can validate them
reliably.

### Required invariants

Validation succeeds only when all of these invariants hold:

1. Every expected current-nonce token occurs exactly once.
2. No well-formed current-nonce token exists outside the expected token table.
3. No marker beginning with a current-nonce `CGCODE` or `CGINLINE` prefix is
   truncated or malformed.
4. Filtering output occurrences to `CGCODE` produces exactly the expected
   `CGCODE` sequence.
5. Every `CGINLINE` occurrence is in the same fenced-code region as its source
   placeholder.

There is deliberately no ordering invariant among `CGINLINE` occurrences
inside one region.

Text containing a different nonce is not interpreted as part of this request.
This preserves the ability to translate documentation that discusses Codeg
placeholder formats and avoids treating unrelated literal examples as model
corruption.

## Validation And Restoration

The output scanner will walk the translated UTF-8 string once. It recognizes
the two complete current-nonce prefixes and reports either a complete token or
a malformed current-nonce marker. It must always advance on malformed input.

Validation maintains:

- an expected-token lookup table;
- an occurrence count per expected token;
- the next expected `CGCODE` ordinal;
- the current output fenced-code region; and
- the source fenced-code region for every `CGINLINE` token.

For each scanned occurrence:

1. Reject a malformed marker.
2. Reject a complete token not present in the expected lookup table.
3. Increment its occurrence count and reject a count greater than one.
4. For `CGCODE`, require the token to equal the next expected fenced token,
   then increment the current output region.
5. For `CGINLINE`, require its source region to equal the current output
   region. Do not compare its position with other inline placeholders.

After the scan, reject any expected token whose count is not one. Only then
restore token occurrences to their exact original strings.

The current replacement loop is safe after these checks because tokens are
unique and each occurs once. The implementation may retain that loop or use a
single-pass reconstruction; this design does not require a restoration
refactor.

## Prompt Contract

The translation prompt will describe the same contract enforced by the host:

```text
Keep every placeholder byte-for-byte unchanged and include each exactly once.
Keep all CGCODE placeholders in their original order.
CGINLINE placeholders may move only when required by target-language grammar;
do not move them across a CGCODE placeholder.
```

The host remains authoritative. Prompt compliance never replaces validation.

## Error Handling

The external behavior remains unchanged:

- type-aware restoration validation failures become the existing
  `DocumentTranslateError::PlaceholderIntegrity`;
- pre-run Markdown protection failures retain their existing generic failure
  path;
- command callers receive the existing task-execution error; and
- the UI continues to display the existing localized
  `translatePlaceholderIntegrityFailed` message.

Internally, integrity failures will carry a content-free classification such
as:

- malformed current-nonce marker;
- unknown current-nonce token;
- missing token;
- duplicate token;
- fenced-code reorder; or
- inline cross-region movement.

Diagnostics may include placeholder kind, expected/found counts, and ordinal
positions. They must not include the document body, translated body, original
code, rendered token, or nonce.

When an otherwise valid result contains inline reordering, a debug diagnostic
may record the number of out-of-order inline positions. Translation still
returns success. No retry is performed.

## Compatibility

- Placeholder strings remain unchanged.
- Markdown inputs with no code placeholders behave as before.
- Markdown inputs with placeholders in original order behave as before.
- Plain-text translation bypasses this protection path and is unchanged.
- Existing API payloads and result types are unchanged.
- Existing i18n keys and UI behavior are unchanged.
- Fenced-code protection, including CRLF and unclosed-fence handling, is
  unchanged.

The only intentional behavior change is that valid same-region inline
reordering is accepted.

## Testing

### Protection unit tests

Add or update focused tests in `document_translate/protect.rs` for:

- two inline placeholders swapped within one region restore successfully;
- the observed `id, data` / `blobs` regression restores correctly;
- multiple inline reorderings within one region restore correctly;
- an inline placeholder moved across a fenced block fails;
- two fenced-code placeholders swapped fail;
- a missing inline or fenced token fails;
- a duplicated inline or fenced token fails;
- an altered or unknown current-nonce index fails;
- a malformed or truncated current-nonce marker fails even if every expected
  token is also present;
- foreign-nonce literal text is ignored by current-request validation; and
- unchanged mixed fenced/inline documents still round-trip exactly.

The current generic `reordered_tokens_fail` test will be split because inline
and fenced reordering now have different contracts.

### Service tests

Add a service regression in `document_translate/service.rs` whose fake agent
returns two same-region inline placeholders in translated order. Assert that
translation succeeds and restores the correct original code spans.

Keep coverage showing that genuine integrity failures still map to
`DocumentTranslateError::PlaceholderIntegrity`.

### Prompt tests

Update `document_translate/types.rs` prompt assertions to cover:

- exactly-once preservation;
- strict `CGCODE` ordering; and
- grammar-only, same-region `CGINLINE` movement.

### Regression checks

Run the narrow Rust library tests for `document_translate::protect`,
`document_translate::types`, and the affected service tests first. Run the
repository's broader Rust checks according to the normal completion policy
after the focused suite passes.

## Rollout And Observability

No migration or feature flag is required. The behavior is local to final
Markdown restoration and does not affect agent launch, session lifecycle, or
transport contracts.

Content-free failure classifications and accepted-inline-reorder counts provide
enough evidence to distinguish model corruption from legitimate grammar
changes. If production data later shows unsafe movement within large regions,
a separate design can introduce finer Markdown structural regions. That is not
part of this change.

## Acceptance Criteria

- The observed Cursor translation with 88 intact tokens and one adjacent
  inline swap succeeds.
- Every expected placeholder is still required exactly once.
- Fenced code blocks cannot reorder.
- Inline code cannot cross a fenced code block.
- Restoration reproduces every protected code span byte-for-byte.
- Genuine integrity violations keep the existing user-visible failure path.
- No source, translation, code span, rendered token, or nonce is added to logs.
- No API, database, frontend, or i18n migration is introduced.
