# Document translate — trial limits & timeout (2026-07-20)

Interim product knobs so code-heavy Markdown can be translated before
streaming preview lands. Full streaming design:
[`2026-07-20-streaming-document-translate-preview-design.md`](./2026-07-20-streaming-document-translate-preview-design.md).

## Shipping values

| Knob | Value | Notes |
| --- | --- | --- |
| **Admission size** | **32 000** Unicode scalars | Measured **after** Markdown code protection (fenced / inline → placeholders). Plain text measured as-is. |
| **Backend deadline** | **480 s** (`DEADLINE_SECS`) | Wall clock from service entry (protect, agent spawn, TTFT, full generation). |
| **FE transport timeout** | **540 s** | Must stay above backend so `translateTimeout` can surface. |
| Output hard cap | 96 000 UTF-8 bytes | Unchanged; fail-closed if stream exceeds. |
| Capacity | 1 | Process-wide; second call → Busy. |

## Why these numbers

- **32k post-protect**: large implementation plans are often mostly fenced code; agent-facing body shrinks sharply after protect. Raw-size 24k rejected many real docs incorrectly.
- **480s**: conservative for max protected body — ~20s first token + ~16k output tokens at ~40 tok/s + margin.
- **540s client**: 60s headroom over backend.

## UX expectations (trial)

- Still **request/response**: toolbar spinner until full result; no progressive draft tab yet (see streaming design PR plan).
- Long waits on large docs are expected; cancel is not yet a first-class mid-run UX (capacity held until runner finishes/cleanup).
- `CodexConfigSchemaGuide`-style files with thousands of **inline** `` `code` `` spans may still exceed 32k **after** protect (placeholders expand short spans).

## Code touchpoints

- `src-tauri/src/document_translate/types.rs` — `MAX_INPUT_SCALARS`, `DEADLINE_SECS`
- `src-tauri/src/document_translate/service.rs` — size check on protected body
- `src/lib/document-translate.ts` — FE mirror of max input
- `src/lib/api.ts` — `translateDocument` `timeoutMs`
