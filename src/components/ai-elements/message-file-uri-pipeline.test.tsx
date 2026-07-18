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

  // Codex / agents often cite sources as `[file](D:/abs/path:line)`. Without
  // rewriting, rehype-harden treats `D:` as a blocked scheme.
  it("renders a bare Windows drive markdown link as a file badge", async () => {
    const { container } = render(
      <MessageResponse>
        {
          "[companion.rs](D:/MyCodeBuddy/src-tauri/src/acp/delegation/companion.rs:1037)"
        }
      </MessageResponse>
    )

    await waitFor(() => {
      expect(
        container.querySelector(
          "button[data-resource-kind='file'][title='/D:/MyCodeBuddy/src-tauri/src/acp/delegation/companion.rs:1037']"
        )
      ).not.toBeNull()
    })
    expect(container.textContent).not.toContain("[blocked]")
    expect(container.textContent).toContain("companion.rs")
  })
})
