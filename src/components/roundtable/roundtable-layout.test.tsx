import { render, screen } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"
import { RoundtableSafeContent } from "./roundtable-safe-content"
import { PreflightConfirmation } from "./preflight-confirmation"

vi.mock("next-intl", () => ({ useTranslations: () => (key: string) => key }))

describe("roundtable narrow-screen content", () => {
  it("lets safe content shrink and wrap long unbroken values", () => {
    const text = "very-long-output".repeat(40)
    render(<RoundtableSafeContent text={text} preview={false} />)
    expect(screen.getByRole("article")).toHaveClass("min-w-0")
    expect(screen.getByText(text)).toHaveClass("[overflow-wrap:anywhere]")
  })

  it("wraps long preflight recipients, hashes, and preview content without hiding them", () => {
    const hash = "a".repeat(64)
    const path = `src/${"long-directory-name".repeat(30)}/file.ts`
    const content = "unbroken-preview".repeat(40)
    const { container } = render(
      <PreflightConfirmation
        recipients={[
          {
            ordinal: 0,
            provider_ref: "provider:1",
            model: "model",
            agent: "codex",
            effort: null,
            origin: `https://${"long-provider-name".repeat(30)}.test`,
          },
        ]}
        moderatorOrdinal={0}
        sourceManifests={[
          {
            hash,
            manifest: {
              manifest_id: "snapshot",
              entries: [
                {
                  path,
                  size: 42,
                  content_hash: hash,
                  text_admissible: true,
                  object: {
                    object_id: hash,
                    content_hash: hash,
                    total_bytes: 42,
                  },
                },
              ],
            },
          },
        ]}
        selectedPaths={[]}
        onPreview={() => undefined}
        sourcePreviews={{ [hash]: content }}
        tools={["read"]}
        network="model_gateway_only"
        writes="scratch_only"
        budget="10"
      />
    )
    expect(container.querySelector("dl")).toHaveClass(
      "min-w-0",
      "[overflow-wrap:anywhere]"
    )
    expect(screen.getByText(content)).toHaveClass("[overflow-wrap:anywhere]")
    expect(
      screen.getByRole("button", { name: `previewSource: ${path}` }).className
    ).toContain("focus-visible:")
    expect(container.textContent).toContain(hash)
    expect(container.textContent).toContain(path)
  })
})

describe("preflight workspace mount", () => {
  const base = {
    recipients: [],
    moderatorOrdinal: 0,
    sourceManifests: [],
    selectedPaths: [],
    tools: [],
    network: "model_gateway_only",
    writes: "scratch_only",
    budget: "1",
  }
  it("shows the read-only mount and warns when it cannot be mounted", () => {
    const { rerender } = render(
      <PreflightConfirmation
        {...base}
        workspaceMount={{
          path: "/workspace-ro",
          access: "read_only",
          available: true,
        }}
      />
    )
    expect(screen.getByText("/workspace-ro")).toBeTruthy()
    expect(screen.getByText("workspaceMountReadOnly")).toBeTruthy()
    expect(screen.queryByRole("alert")).toBeNull()
    rerender(
      <PreflightConfirmation
        {...base}
        workspaceMount={{
          path: "/workspace-ro",
          access: "read_only",
          available: false,
        }}
      />
    )
    expect(screen.getByRole("alert").textContent).toBe(
      "workspaceMountUnavailable"
    )
  })
})
