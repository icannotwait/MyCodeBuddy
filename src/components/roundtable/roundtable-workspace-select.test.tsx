import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { webcrypto } from "node:crypto"
import { roundtableHash } from "@/lib/roundtable/api"
import { RoundtableWorkbench } from "./roundtable-workbench"
import {
  orderRoundtableWorkspaces,
  RoundtableWorkspaceSelect,
} from "./roundtable-workspace-select"

const call = vi.hoisted(() => vi.fn())
const folders = vi.hoisted(() => [
  {
    id: 7,
    name: "project",
    path: "/repo/project",
    kind: "regular",
    parent_id: null,
    alias: null,
  },
  {
    id: 8,
    name: "project-wt",
    path: "/repo/project/.worktrees/wt",
    kind: "regular",
    parent_id: 7,
    alias: null,
  },
  {
    id: 9,
    name: "other",
    path: "/repo/other",
    kind: "regular",
    parent_id: null,
    alias: "Other",
  },
  {
    id: 11,
    name: "chat",
    path: "/tmp/chat",
    kind: "chat",
    parent_id: null,
    alias: null,
  },
])
const listAllFolderDetails = vi.hoisted(() => vi.fn())
vi.mock("./roundtable-topic-input")
vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call, subscribe: async () => () => undefined }),
}))
vi.mock("@/lib/api", () => ({
  listModelProviders: async () => [],
  listAllFolderDetails,
}))
vi.mock("next-intl", () => ({
  useTranslations: () => (key: string, values?: Record<string, unknown>) =>
    values ? `${key} ${JSON.stringify(values)}` : key,
  useLocale: () => "en",
}))

const workspaceSelect = () =>
  screen.getByRole("combobox", { name: "workspace" })

describe("roundtable workspace selector", () => {
  beforeEach(() => {
    call.mockReset()
    listAllFolderDetails.mockReset()
    listAllFolderDetails.mockResolvedValue(folders)
    vi.stubGlobal("crypto", webcrypto)
  })
  afterEach(() => {
    cleanup()
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  })

  it("orders worktrees under their repository", () => {
    expect(
      orderRoundtableWorkspaces([folders[1], folders[2], folders[0]]).map(
        ({ workspace, nested }) => [workspace.id, nested]
      )
    ).toEqual([
      [9, false],
      [7, false],
      [8, true],
    ])
  })

  it("defaults to the URL workspace, switches in place and creates the room there", async () => {
    const replaceState = vi.spyOn(window.history, "replaceState")
    call.mockImplementation(
      async (
        command: string,
        args: {
          request: {
            config: {
              participants: {
                ordinal: number
                provider_ref: string
                agent: string
              }[]
            }
          }
        }
      ) => {
        if (command === "roundtable_list") return { rooms: [], cursor: null }
        if (command === "roundtable_preflight") {
          const config = args.request.config
          return {
            enabled: true,
            readiness: "ready",
            tools: [],
            network: "model_gateway_only",
            writes: "scratch_only",
            confirmed_preflight_id: "fixed",
            config_hash: await roundtableHash(config),
            capability: {
              recipients: config.participants.map((participant) => ({
                ordinal: participant.ordinal,
                provider_ref: participant.provider_ref,
                agent: participant.agent,
                model: "model-x",
                origin: "binding",
              })),
            },
            error: null,
          }
        }
        if (command === "roundtable_create") return new Promise(() => undefined)
        throw new Error(`Unexpected command: ${command}`)
      }
    )
    render(<RoundtableWorkbench workspaceId="7" />)
    await waitFor(() => expect(workspaceSelect()).toHaveValue("7"))
    const options = within(workspaceSelect())
      .getAllByRole("option")
      .map((option) => option.textContent)
    // Chat scratch folders are not workspaces a room can belong to.
    expect(options).toEqual([
      "project",
      "\u00a0\u00a0↳ project-wt",
      "Other [ other ]",
    ])
    expect(screen.getByText("/repo/project")).toBeInTheDocument()
    expect(call).toHaveBeenCalledWith("roundtable_list", {
      request: expect.objectContaining({ workspace_id: "7" }),
    })

    fireEvent.change(screen.getByLabelText("topic"), {
      target: { value: "Keep this question" },
    })
    fireEvent.change(workspaceSelect(), { target: { value: "9" } })

    expect(replaceState).toHaveBeenLastCalledWith(
      null,
      "",
      "/roundtable?workspace_id=9"
    )
    expect(workspaceSelect()).toHaveValue("9")
    // The form survives the switch; source paths now resolve in the new root.
    expect(screen.getByLabelText("topic")).toHaveValue("Keep this question")
    expect(screen.getByText("/repo/other")).toBeInTheDocument()
    expect(
      screen.getByText(
        `sourceRootHelp ${JSON.stringify({ path: "/repo/other" })}`
      )
    ).toBeInTheDocument()
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("roundtable_list", {
        request: expect.objectContaining({ workspace_id: "9" }),
      })
    )

    fireEvent.click(screen.getByRole("button", { name: "preflight" }))
    await screen.findByLabelText("confirm")
    expect(call).toHaveBeenCalledWith("roundtable_preflight", {
      request: {
        config: expect.objectContaining({ workspace_id: "9" }),
      },
    })
    fireEvent.click(screen.getByLabelText("confirm"))
    const create = screen.getByRole("button", { name: "create" })
    await waitFor(() => expect(create).toBeEnabled())
    fireEvent.click(create)
    await waitFor(() =>
      expect(
        call.mock.calls.find(([command]) => command === "roundtable_create")
      ).toBeTruthy()
    )
    const [, created] = call.mock.calls.find(
      ([command]) => command === "roundtable_create"
    ) as [string, { request: { config: { workspace_id: string } } }]
    expect(created.request.config.workspace_id).toBe("9")
    // A create in flight pins the workspace.
    expect(workspaceSelect()).toBeDisabled()
  })

  it("requires choosing a workspace when the URL has none", async () => {
    call.mockResolvedValue({ rooms: [], cursor: null })
    render(<RoundtableWorkbench workspaceId="" />)
    await waitFor(() => expect(workspaceSelect()).toBeEnabled())
    expect(workspaceSelect()).toHaveValue("")
    expect(screen.getByText("workspaceRequired")).toBeInTheDocument()
    fireEvent.change(screen.getByLabelText("topic"), {
      target: { value: "Question" },
    })
    expect(screen.getByRole("button", { name: "preflight" })).toBeDisabled()
    fireEvent.change(workspaceSelect(), { target: { value: "7" } })
    expect(screen.getByRole("button", { name: "preflight" })).toBeEnabled()
  })

  it("flags a workspace from the URL that no longer exists", async () => {
    call.mockResolvedValue({ rooms: [], cursor: null })
    render(<RoundtableWorkbench workspaceId="42" />)
    fireEvent.change(screen.getByLabelText("topic"), {
      target: { value: "Question" },
    })
    expect(await screen.findByText("workspaceUnavailable")).toBeInTheDocument()
    expect(
      screen.getByRole("option", {
        name: `workspaceUnknown ${JSON.stringify({ id: "42" })}`,
      })
    ).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "preflight" })).toBeDisabled()
  })

  it("shows loading, empty and error states", () => {
    const onRetry = vi.fn()
    const props = { value: "", onChange: vi.fn(), onRetry }
    const { rerender } = render(
      <RoundtableWorkspaceSelect
        workspaces={null}
        loading
        error={null}
        {...props}
      />
    )
    expect(workspaceSelect()).toBeDisabled()
    expect(screen.getByRole("status")).toHaveTextContent("workspacesLoading")

    rerender(
      <RoundtableWorkspaceSelect
        workspaces={[]}
        loading={false}
        error={null}
        {...props}
      />
    )
    expect(screen.getByRole("status")).toHaveTextContent("workspacesEmpty")
    expect(screen.getByRole("link", { name: "openFolder" })).toHaveAttribute(
      "href",
      "/workspace"
    )

    rerender(
      <RoundtableWorkspaceSelect
        workspaces={null}
        loading={false}
        error="offline"
        {...props}
      />
    )
    expect(screen.getByRole("alert")).toHaveTextContent(
      "workspacesError offline"
    )
    fireEvent.click(screen.getByRole("button", { name: "retryWorkspaces" }))
    expect(onRetry).toHaveBeenCalledOnce()
  })

  it("lets the workbench retry a failed workspace list", async () => {
    call.mockResolvedValue({ rooms: [], cursor: null })
    listAllFolderDetails.mockRejectedValueOnce(new Error("offline"))
    render(<RoundtableWorkbench workspaceId="7" />)
    fireEvent.click(
      await screen.findByRole("button", { name: "retryWorkspaces" })
    )
    await waitFor(() => expect(workspaceSelect()).toHaveValue("7"))
    expect(screen.getByText("/repo/project")).toBeInTheDocument()
  })
})
