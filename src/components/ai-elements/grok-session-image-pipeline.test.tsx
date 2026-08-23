import { render, screen, waitFor } from "@testing-library/react"
import type { ComponentProps } from "react"
import { beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => ({
  imageProps: [] as Record<string, unknown>[],
}))

vi.mock("./grok-session-image", () => ({
  GrokSessionImage: (
    props: { src?: string; alt?: string } & Record<string, unknown>
  ) => {
    mocks.imageProps.push(props)
    return (
      <span
        data-testid="grok-private-image"
        data-src={props.src}
        data-alt={props.alt}
      />
    )
  },
}))

// MarkdownLink requires workspace opener context even when a link is never
// clicked. Keep that unrelated hook inert while exercising the real
// MarkdownLink and real Streamdown sanitize/harden pipeline below.
vi.mock("@/components/ai-elements/link-safety", async (importOriginal) => ({
  ...(await importOriginal<
    typeof import("@/components/ai-elements/link-safety")
  >()),
  useStreamdownLinkSafety: () => ({ enabled: false }),
}))

import {
  GrokConversationProvider,
  GrokSessionImageScope,
} from "./grok-session-image-context"
import { MessageResponse } from "./message"

type MessageComponents = NonNullable<
  ComponentProps<typeof MessageResponse>["components"]
>

function ScopedMessage({
  children,
  active = true,
  ...props
}: ComponentProps<typeof MessageResponse> & { active?: boolean }) {
  return (
    <GrokConversationProvider conversationId={42}>
      <GrokSessionImageScope phase={active ? "complete" : null}>
        <MessageResponse {...props}>{children}</MessageResponse>
      </GrokSessionImageScope>
    </GrokConversationProvider>
  )
}

describe("MessageResponse Grok session image pipeline", () => {
  beforeEach(() => {
    mocks.imageProps.length = 0
  })

  it("renders a valid local image through the private tag only in active scope", async () => {
    function Harness({ active }: { active: boolean }) {
      return (
        <GrokConversationProvider conversationId={42}>
          <GrokSessionImageScope phase={active ? "complete" : null}>
            <MessageResponse>{"![目标](images/2.png)"}</MessageResponse>
          </GrokSessionImageScope>
        </GrokConversationProvider>
      )
    }

    const { rerender } = render(<Harness active />)
    expect(await screen.findByTestId("grok-private-image")).toHaveAttribute(
      "data-src",
      "images/2.png"
    )

    rerender(<Harness active={false} />)
    await waitFor(() => {
      expect(screen.queryByTestId("grok-private-image")).toBeNull()
    })
    expect(screen.getByText(/Image blocked/i)).toBeInTheDocument()
  })

  it("leaves remote images on Streamdown's default img component", async () => {
    render(
      <ScopedMessage>{"![remote](https://example.com/a.png)"}</ScopedMessage>
    )

    expect(screen.queryByTestId("grok-private-image")).toBeNull()
    expect(await screen.findByRole("img", { name: "remote" })).toHaveAttribute(
      "src",
      "https://example.com/a.png"
    )
  })

  it.each([
    ["invalid local", "docs/foo.png"],
    ["SVG", "images/a.svg"],
    ["nested path", "images/a/b.png"],
  ])(
    "blocks a %s image instead of using the private component",
    async (_, src) => {
      render(<ScopedMessage>{`![invalid](${src})`}</ScopedMessage>)

      expect(screen.queryByTestId("grok-private-image")).toBeNull()
      expect(await screen.findByText(/Image blocked/i)).toBeInTheDocument()
    }
  )

  it("strips model-authored image props before invoking the private component", async () => {
    render(
      <ScopedMessage>
        {
          '<img src="images/2.png" alt="exact" title="drop" onerror="drop" data-model="drop">'
        }
      </ScopedMessage>
    )

    expect(await screen.findByTestId("grok-private-image")).toHaveAttribute(
      "data-alt",
      "exact"
    )
    const received = mocks.imageProps.find(
      (props) => props.src === "images/2.png"
    )
    expect(received).toMatchObject({ src: "images/2.png", alt: "exact" })
    expect(received).not.toHaveProperty("title")
    expect(received).not.toHaveProperty("onerror")
    expect(received).not.toHaveProperty("data-model")
  })

  it("model_authored_private_tag_is_removed_before_component_mapping", async () => {
    const { container } = render(
      <ScopedMessage>
        {
          '<codeg-grok-session-image src="images/2.png" alt="x"></codeg-grok-session-image>'
        }
      </ScopedMessage>
    )

    await waitFor(() => {
      expect(container.querySelector("p")).not.toBeNull()
    })
    expect(screen.queryByTestId("grok-private-image")).toBeNull()
  })

  it("keeps the local MarkdownLink authoritative with its unchanged href", async () => {
    const components = {
      a: () => <span data-testid="caller-anchor" />,
    } as MessageComponents

    render(
      <ScopedMessage components={components}>
        {"[local](docs/foo.ts)"}
      </ScopedMessage>
    )

    expect(
      await screen.findByRole("button", { name: "file: local" })
    ).toHaveAttribute("title", "docs/foo.ts")
    expect(screen.getByRole("button", { name: "file: local" })).toHaveAttribute(
      "data-resource-kind",
      "file"
    )
    expect(screen.queryByTestId("caller-anchor")).toBeNull()
  })

  it("caller_rehype_plugins_cannot_disable_the_scoped_pipeline", async () => {
    render(
      <ScopedMessage rehypePlugins={[]}>
        {"![目标](images/2.png)"}
      </ScopedMessage>
    )

    expect(await screen.findByTestId("grok-private-image")).toHaveAttribute(
      "data-src",
      "images/2.png"
    )
  })

  it("keeps the app private component authoritative over caller components", async () => {
    const components = {
      "codeg-grok-session-image": () => (
        <span data-testid="caller-private-image" />
      ),
    } as MessageComponents

    render(
      <ScopedMessage components={components}>
        {"![目标](images/2.png)"}
      </ScopedMessage>
    )

    expect(await screen.findByTestId("grok-private-image")).toBeInTheDocument()
    expect(screen.queryByTestId("caller-private-image")).toBeNull()
  })
})
