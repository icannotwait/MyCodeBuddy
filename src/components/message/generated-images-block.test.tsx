import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it } from "vitest"

import { GeneratedImagesBlock } from "./generated-images-block"
import enMessages from "@/i18n/messages/en.json"
import type { UserImageDisplay } from "@/lib/adapters/ai-elements-adapter"

const image: UserImageDisplay = {
  name: "page-capture.png",
  data: "QUJD",
  mime_type: "image/png",
  uri: null,
}

function renderCard(props: Partial<{ label: string | null }> = {}) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <GeneratedImagesBlock revisedPrompt={null} image={image} {...props} />
    </NextIntlClientProvider>
  )
}

describe("GeneratedImagesBlock heading", () => {
  it("keeps the translated generation copy when no label is passed", () => {
    // codex image generation is the one caller that leaves `label` unset.
    renderCard()
    expect(screen.getByText("Image generation")).toBeInTheDocument()
  })

  it("falls back to the generation copy for a blank label", () => {
    renderCard({ label: "   " })
    expect(screen.getByText("Image generation")).toBeInTheDocument()
  })

  it("shows the tool/page name when one is passed", () => {
    renderCard({ label: "Page Capture" })
    expect(screen.getByText("Page Capture")).toBeInTheDocument()
    expect(screen.queryByText("Image generation")).not.toBeInTheDocument()
  })

  it("shows omission copy for a completed snapshot without a preview", () => {
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <GeneratedImagesBlock
          revisedPrompt={null}
          image={null}
          status="completed"
          previewOmitted
        />
      </NextIntlClientProvider>
    )
    expect(
      screen.getByText(
        "The image preview was not included in this snapshot."
      )
    ).toBeInTheDocument()
    expect(screen.queryByText("Image generation failed")).not.toBeInTheDocument()
    expect(
      screen.queryByRole("button", { name: "Download image" })
    ).not.toBeInTheDocument()
  })

  it("still shows a real failure when the tool failed and the preview was omitted", () => {
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <GeneratedImagesBlock
          revisedPrompt={null}
          image={null}
          status="failed"
          previewOmitted
        />
      </NextIntlClientProvider>
    )
    expect(screen.getByText("Image generation failed")).toBeInTheDocument()
    expect(
      screen.queryByText(
        "The image preview was not included in this snapshot."
      )
    ).not.toBeInTheDocument()
  })

  it("treats a completed empty result without the omission flag as a failure", () => {
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <GeneratedImagesBlock
          revisedPrompt={null}
          image={null}
          status="completed"
        />
      </NextIntlClientProvider>
    )
    expect(screen.getByText("Image generation failed")).toBeInTheDocument()
    expect(
      screen.queryByText(
        "The image preview was not included in this snapshot."
      )
    ).not.toBeInTheDocument()
  })

  it("bounds a long heading to one line and keeps the full text reachable", () => {
    // The heading is agent-authored now, so it can be arbitrarily long.
    const long = "Read file '/Users/x/very/long/path/to/a-page-capture.png'"
    renderCard({ label: long })
    const heading = screen.getByText(long)
    expect(heading.className).toContain("truncate")
    expect(heading).toHaveAttribute("title", long)
  })
})
