import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import messages from "@/i18n/messages/en.json"

const call = vi.fn()
const copy = vi.fn()
vi.mock("next-intl", () => ({
  useLocale: () => "en",
  useTranslations:
    () =>
    (key: keyof typeof messages.Roundtable, values?: Record<string, string>) =>
      Object.entries(values ?? {}).reduce(
        (text, [name, value]) => text.replace(`{${name}}`, value),
        messages.Roundtable[key] as string
      ),
}))
vi.mock("@/lib/transport", () => ({ getTransport: () => ({ call }) }))
vi.mock("@/lib/platform", () => ({ isDesktop: () => false }))
vi.mock("@/lib/utils", async (original) => ({
  ...(await original<typeof import("@/lib/utils")>()),
  copyTextToClipboard: (text: string) => copy(text),
}))

import { RoundtableConclusionActions } from "./roundtable-conclusion-actions"

const exported = {
  room_id: "room-1",
  message_id: "m",
  body_hash: "h",
  file_name: "2026-10-09-topic-room0001.md",
  relative_path: "docs/roundtable/2026-10-09-topic-room0001.md",
  markdown: "# Roundtable conclusion\n",
  redactions: 0,
  workspace_available: true,
  saves: [],
}

function conflict() {
  return Object.assign(new Error("conflict"), {
    details: { reason: "already_exists", field_errors: [] },
  })
}

describe("roundtable conclusion actions", () => {
  beforeEach(() => {
    call.mockReset()
    copy.mockReset()
  })

  it("saves, then offers overwrite or save-as when the file exists", async () => {
    const saves: Array<Record<string, unknown>> = []
    call.mockImplementation(
      async (command: string, args: { request: Record<string, unknown> }) => {
        if (command === "roundtable_conclusion_export") return exported
        saves.push(args.request)
        if (args.request.mode === "create") throw conflict()
        return {
          request_id: args.request.request_id,
          mode: args.request.mode,
          relative_path: "docs/roundtable/2026-10-09-topic-room0001-2.md",
          status: "created",
        }
      }
    )
    render(<RoundtableConclusionActions roomId="room-1" />)
    fireEvent.click(screen.getByRole("button", { name: "Save to workspace" }))
    expect(await screen.findByRole("alertdialog")).toBeTruthy()
    expect(
      screen.getByText(/already exists in this workspace/).textContent
    ).toContain(exported.relative_path)
    fireEvent.click(screen.getByRole("button", { name: "Save as copy" }))
    expect(
      await screen.findByText(
        "Saved to docs/roundtable/2026-10-09-topic-room0001-2.md"
      )
    ).toBeTruthy()
    expect(screen.queryByRole("alertdialog")).toBeNull()
    expect(saves.map((save) => save.mode)).toEqual(["create", "save_as"])
    expect(saves[0].request_id).not.toBe(saves[1].request_id)
    expect(saves.every((save) => save.room_id === "room-1")).toBe(true)
    expect(saves.some((save) => "path" in save || "root" in save)).toBe(false)
  })

  it("copies and downloads the server-rendered markdown", async () => {
    call.mockResolvedValue(exported)
    copy.mockResolvedValue(true)
    const click = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {})
    URL.createObjectURL = vi.fn(() => "blob:x")
    URL.revokeObjectURL = vi.fn()
    render(<RoundtableConclusionActions roomId="room-1" />)
    fireEvent.click(screen.getByRole("button", { name: "Copy" }))
    await waitFor(() => expect(copy).toHaveBeenCalledWith(exported.markdown))
    expect(
      await screen.findByText("Copied the conclusion as Markdown.")
    ).toBeTruthy()
    fireEvent.click(screen.getByRole("button", { name: "Download .md" }))
    expect(
      await screen.findByText(`Downloaded ${exported.file_name}`)
    ).toBeTruthy()
    expect(click).toHaveBeenCalledOnce()
    click.mockRestore()
  })

  it("shows the last saved path and disables saving without a workspace", async () => {
    call.mockResolvedValue({
      ...exported,
      workspace_available: false,
      saves: [{ relative_path: "docs/roundtable/old.md" }],
    })
    render(<RoundtableConclusionActions roomId="room-1" />)
    expect(await screen.findByText("docs/roundtable/old.md")).toBeTruthy()
    expect(
      (
        screen.getByRole("button", {
          name: "Save to workspace",
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true)
  })
})
