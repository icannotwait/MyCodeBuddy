import { fireEvent, render, screen } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"

import { CreateRoundtable } from "@/components/roundtable/create-roundtable"
import { PreflightConfirmation } from "@/components/roundtable/preflight-confirmation"
import messages from "@/i18n/messages/zh-CN.json"
vi.mock("./roundtable-topic-input")
vi.mock("next-intl", () => ({
  useTranslations: () => (key: keyof typeof messages.Roundtable) =>
    messages.Roundtable[key],
}))

describe("create roundtable", () => {
  it("dispatches confirmed start and leaves disabled execution unavailable", () => {
    const start = vi.fn()
    const { rerender } = render(
      <CreateRoundtable
        mode="parallel"
        confirmed
        onConfirm={() => undefined}
        onStart={start}
        enabled={false}
      />
    )
    fireEvent.click(screen.getByRole("button", { name: "开始" }))
    expect(start).not.toHaveBeenCalled()
    rerender(
      <CreateRoundtable
        mode="parallel"
        confirmed
        onConfirm={() => undefined}
        onStart={start}
        enabled
      />
    )
    fireEvent.click(screen.getByRole("button", { name: "开始" }))
    expect(start).toHaveBeenCalledOnce()
  })
  it("shows the consultation choice and holds start until confirmation", () => {
    render(
      <CreateRoundtable
        mode="parallel"
        quotaError="额度不足"
        confirmed={false}
        onConfirm={() => undefined}
      />
    )
    expect(screen.getByText("并行咨询")).toBeTruthy()
    expect(screen.getByRole("alert").textContent).toContain("额度不足")
    expect(screen.getAllByRole("button")[1].hasAttribute("disabled")).toBe(true)
    render(
      <PreflightConfirmation
        recipients={[
          {
            ordinal: 0,
            provider_ref: "provider:1",
            model: "actual",
            origin: "https://provider.test",
            agent: "codex",
            effort: null,
          },
        ]}
        moderatorOrdinal={0}
        sourceManifests={[]}
        selectedPaths={[]}
        tools={["read"]}
        network="none"
        writes="none"
        budget="10"
      />
    )
    expect(
      screen.getByText("https://provider.test", { exact: false })
    ).toBeTruthy()
    expect(screen.getByText("10")).toBeTruthy()
  })
})

it("discloses resolved recipients and immutable selected source hashes", () => {
  render(
    <PreflightConfirmation
      recipients={[
        {
          ordinal: 0,
          provider_ref: "provider:9",
          model: "actual-model",
          origin: "https://provider.example",
          agent: "grok",
          effort: "high",
        },
      ]}
      moderatorOrdinal={0}
      sourceManifests={[
        {
          hash: "manifest-hash",
          manifest: {
            manifest_id: "snapshot",
            entries: [
              {
                path: "src/selected.ts",
                size: 42,
                content_hash: "content-hash",
                text_admissible: true,
                object: {
                  object_id: "content-hash",
                  content_hash: "content-hash",
                  total_bytes: 42,
                },
              },
            ],
          },
        },
      ]}
      selectedPaths={[]}
      sourcePreviews={{
        "content-hash": "<script>fixture</script> sk-fake-preview",
      }}
      tools={["read"]}
      network="model_gateway_only"
      writes="scratch_only"
      budget="10"
    />
  )
  expect(
    screen.getByText("<script>fixture</script> sk-fake-preview")
  ).toBeTruthy()
  expect(document.querySelector("script")).toBeNull()
  for (const text of [
    "provider:9",
    "actual-model",
    "https://provider.example",
    "grok",
    "high",
    "src/selected.ts",
    "content-hash",
    "manifest-hash",
  ]) {
    expect(screen.getByText(text, { exact: false })).toBeTruthy()
  }
})
