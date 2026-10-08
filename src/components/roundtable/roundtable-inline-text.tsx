import { Fragment } from "react"

const CODE_SPAN = /(`[^`\n]+`)/g

/**
 * Render a plain-text title with inline code spans (`like this`) as <code>.
 * Everything else stays text, so no HTML or links can come from the topic.
 */
export function RoundtableInlineText({ text }: { text: string }) {
  const parts = text.split(CODE_SPAN)
  return (
    <>
      {parts.map((part, index) =>
        index % 2 === 1 ? (
          <code
            key={index}
            className="rounded bg-muted px-1 py-0.5 font-mono text-[0.88em] font-normal [overflow-wrap:anywhere]"
          >
            {part.slice(1, -1)}
          </code>
        ) : (
          <Fragment key={index}>{part}</Fragment>
        )
      )}
    </>
  )
}

/** Title-attribute text: the topic without code-span backticks. */
export function plainInlineText(text: string) {
  return text.replace(CODE_SPAN, (span) => span.slice(1, -1))
}
