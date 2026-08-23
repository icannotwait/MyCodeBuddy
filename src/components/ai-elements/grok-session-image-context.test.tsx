import { render, screen } from "@testing-library/react"
import { expect, it } from "vitest"

import {
  GrokConversationProvider,
  GrokSessionImageScope,
  useGrokConversationId,
  useGrokSessionImageScope,
} from "./grok-session-image-context"

function Probe() {
  const conversationId = useGrokConversationId()
  const scope = useGrokSessionImageScope()
  return <output>{JSON.stringify({ conversationId, scope })}</output>
}

it("activates only a positive conversation inside an explicit phase scope", () => {
  const { rerender } = render(
    <GrokConversationProvider conversationId={42}>
      <GrokSessionImageScope phase="live">
        <Probe />
      </GrokSessionImageScope>
    </GrokConversationProvider>
  )
  expect(screen.getByText(/"conversationId":42/)).toHaveTextContent(
    '"scope":{"conversationId":42,"phase":"live"}'
  )
  rerender(
    <GrokConversationProvider conversationId={-1}>
      <GrokSessionImageScope phase="complete">
        <Probe />
      </GrokSessionImageScope>
    </GrokConversationProvider>
  )
  expect(screen.getByText(/"conversationId":null/)).toHaveTextContent(
    '"scope":null'
  )
})

it("conversation context alone does not activate Markdown", () => {
  render(
    <GrokConversationProvider conversationId={42}>
      <Probe />
    </GrokConversationProvider>
  )
  expect(screen.getByText(/"scope":null/)).toBeInTheDocument()
})

it("an explicit null inner scope blocks accidental outer inheritance", () => {
  render(
    <GrokConversationProvider conversationId={42}>
      <GrokSessionImageScope phase="live">
        <GrokSessionImageScope phase={null}>
          <Probe />
        </GrokSessionImageScope>
      </GrokSessionImageScope>
    </GrokConversationProvider>
  )
  expect(screen.getByText(/"conversationId":42/)).toHaveTextContent(
    '"scope":null'
  )
})
