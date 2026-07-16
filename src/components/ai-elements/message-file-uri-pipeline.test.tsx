import { render, waitFor } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"

vi.mock("@/components/ai-elements/link-safety", () => ({
  useStreamdownLinkSafety: () => ({ enabled: false }),
}))

import { MessageResponse } from "./message"

describe("MessageResponse Windows file URI pipeline", () => {
  it("renders a Windows file URI as a file badge without [blocked]", async () => {
    const { container } = render(
      <MessageResponse>
        {"[app.ts](file:///C:/repo/src/app.ts#L12)"}
      </MessageResponse>
    )

    await waitFor(() => {
      expect(
        container.querySelector(
          "button[data-resource-kind='file'][title='/C:/repo/src/app.ts#L12']"
        )
      ).not.toBeNull()
    })
    expect(container.textContent).not.toContain("[blocked]")
  })
})
