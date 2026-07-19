import { fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"

import { DelegationCardChrome } from "./delegation-card-chrome"
import enMessages from "@/i18n/messages/en.json"
import type { EditRollupViewModel } from "@/lib/delegation-card"
import type {
  AttentionRequestSummary,
  DelegationRuntimeStats,
} from "@/lib/types"

function renderWithIntl(ui: React.ReactElement) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      {ui}
    </NextIntlClientProvider>
  )
}

const ATTENTION: AttentionRequestSummary = {
  request_id: "req-1",
  task_id: "task-1",
  message: "Need parent to choose approach A or B",
  created_at: "2026-07-19T00:01:00.000Z",
}

function statsOf(
  overrides: Partial<DelegationRuntimeStats> = {}
): DelegationRuntimeStats {
  return {
    started_at: "2026-07-19T00:00:00.000Z",
    tool_call_count: 0,
    edit_tool_call_count: 0,
    touched_files: [],
    touched_files_truncated: false,
    line_counts_complete: false,
    ...overrides,
  }
}

const omitRollup: EditRollupViewModel = { mode: "omit" }

describe("DelegationCardChrome", () => {
  it("renders secondary text with title tooltip preference", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary="Auto title for audit"
        conversationTitle="Auto title for audit"
        task="raw task prompt"
        elapsedMs={null}
        toolCallCount={null}
        editRollup={omitRollup}
        attentionRequest={null}
        runtimeStats={null}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    const secondary = screen.getByTestId("delegation-secondary")
    expect(secondary).toHaveTextContent("Auto title for audit")
    expect(secondary).toHaveAttribute("title", "Auto title for audit")
  })

  it("falls secondary tooltip back to task when title is empty", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary="summarize failures"
        conversationTitle={null}
        task="summarize failures"
        elapsedMs={null}
        toolCallCount={null}
        editRollup={omitRollup}
        attentionRequest={null}
        runtimeStats={null}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    expect(screen.getByTestId("delegation-secondary")).toHaveAttribute(
      "title",
      "summarize failures"
    )
  })

  it("omits secondary when displaySecondary is null", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary={null}
        elapsedMs={null}
        toolCallCount={null}
        editRollup={omitRollup}
        attentionRequest={null}
        runtimeStats={null}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    expect(screen.queryByTestId("delegation-secondary")).not.toBeInTheDocument()
  })

  it("shows attention badge with localized parent-decision label", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary={null}
        elapsedMs={null}
        toolCallCount={null}
        editRollup={omitRollup}
        attentionRequest={ATTENTION}
        runtimeStats={null}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    const badge = screen.getByTestId("delegation-attention-badge")
    expect(badge).toHaveTextContent("Waiting for parent decision")
    expect(badge).toHaveAttribute(
      "title",
      "Need parent to choose approach A or B"
    )
  })

  it("builds operational line from present segments only (no empty slots)", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary={null}
        elapsedMs={90_000}
        toolCallCount={18}
        editRollup={{
          mode: "files",
          fileCount: 4,
          fileCountTruncated: false,
          additions: 126,
          deletions: 34,
          showLineTotals: true,
        }}
        attentionRequest={null}
        runtimeStats={statsOf({
          tool_call_count: 18,
          touched_files: [
            {
              path: "src/a.ts",
              outside_workspace: false,
              additions: 10,
              deletions: 2,
            },
          ],
        })}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    const ops = screen.getByTestId("delegation-operational")
    expect(ops).toHaveTextContent("1m 30s")
    expect(ops).toHaveTextContent("18 tool uses")
    expect(ops).toHaveTextContent("4 files")
    expect(ops).toHaveTextContent("+126 -34")
    // Joined with " | " — full tooltip carries the joined line.
    expect(ops.querySelector("[title]")?.getAttribute("title")).toBe(
      "1m 30s | 18 tool uses | 4 files +126 -34"
    )
  })

  it("omits tool segment when toolCallCount is null (missing stats)", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary={null}
        elapsedMs={12_000}
        toolCallCount={null}
        editRollup={omitRollup}
        attentionRequest={null}
        runtimeStats={null}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    const ops = screen.getByTestId("delegation-operational")
    expect(ops).toHaveTextContent("12s")
    expect(ops).not.toHaveTextContent("tool")
  })

  it("shows zero tool uses only when stats are present with count 0", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary={null}
        elapsedMs={null}
        toolCallCount={0}
        editRollup={omitRollup}
        attentionRequest={null}
        runtimeStats={statsOf({ tool_call_count: 0 })}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    expect(screen.getByTestId("delegation-operational")).toHaveTextContent(
      "0 tool uses"
    )
  })

  it("renders edit-calls segment without claiming file count", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary={null}
        elapsedMs={null}
        toolCallCount={3}
        editRollup={{ mode: "editCalls", editCallCount: 2 }}
        attentionRequest={null}
        runtimeStats={statsOf({
          tool_call_count: 3,
          edit_tool_call_count: 2,
        })}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    const ops = screen.getByTestId("delegation-operational")
    expect(ops).toHaveTextContent("2 edit calls")
    expect(ops).not.toHaveTextContent("file")
  })

  it("renders 200+ files when fileCountTruncated", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary={null}
        elapsedMs={null}
        toolCallCount={null}
        editRollup={{
          mode: "files",
          fileCount: 200,
          fileCountTruncated: true,
          additions: null,
          deletions: null,
          showLineTotals: false,
        }}
        attentionRequest={null}
        runtimeStats={statsOf({
          touched_files_truncated: true,
          touched_files: Array.from({ length: 200 }, (_, i) => ({
            path: `f${i}.ts`,
            outside_workspace: false,
          })),
        })}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    expect(screen.getByTestId("delegation-operational")).toHaveTextContent(
      "200+ files"
    )
  })

  it("does not render expand toggle when touched_files is empty", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary={null}
        elapsedMs={5_000}
        toolCallCount={1}
        editRollup={{ mode: "editCalls", editCallCount: 1 }}
        attentionRequest={null}
        runtimeStats={statsOf({
          tool_call_count: 1,
          edit_tool_call_count: 1,
          touched_files: [],
        })}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    expect(
      screen.queryByTestId("delegation-files-toggle")
    ).not.toBeInTheDocument()
    expect(
      screen.queryByTestId("delegation-files-panel")
    ).not.toBeInTheDocument()
  })

  it("does not render expand toggle when runtimeStats is null", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary="task only"
        elapsedMs={null}
        toolCallCount={null}
        editRollup={omitRollup}
        attentionRequest={null}
        runtimeStats={null}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    expect(
      screen.queryByTestId("delegation-files-toggle")
    ).not.toBeInTheDocument()
  })

  it("expands paths, outside_workspace marker, line totals, and truncation notice", () => {
    const onToggle = vi.fn()
    const runtimeStats = statsOf({
      touched_files_truncated: true,
      touched_files: [
        {
          path: "src/in-workspace.ts",
          outside_workspace: false,
          additions: 3,
          deletions: 1,
        },
        {
          path: "/tmp/outside.log",
          outside_workspace: true,
        },
      ],
    })
    const { rerender } = renderWithIntl(
      <DelegationCardChrome
        displaySecondary={null}
        elapsedMs={null}
        toolCallCount={2}
        editRollup={{
          mode: "files",
          fileCount: 2,
          fileCountTruncated: true,
          additions: null,
          deletions: null,
          showLineTotals: false,
        }}
        attentionRequest={null}
        runtimeStats={runtimeStats}
        filesExpanded={false}
        onToggleFilesExpanded={onToggle}
      />
    )

    expect(
      screen.queryByTestId("delegation-files-panel")
    ).not.toBeInTheDocument()
    fireEvent.click(screen.getByTestId("delegation-files-toggle"))
    expect(onToggle).toHaveBeenCalledTimes(1)

    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <DelegationCardChrome
          displaySecondary={null}
          elapsedMs={null}
          toolCallCount={2}
          editRollup={{
            mode: "files",
            fileCount: 2,
            fileCountTruncated: true,
            additions: null,
            deletions: null,
            showLineTotals: false,
          }}
          attentionRequest={null}
          runtimeStats={runtimeStats}
          filesExpanded
          onToggleFilesExpanded={onToggle}
        />
      </NextIntlClientProvider>
    )

    const panel = screen.getByTestId("delegation-files-panel")
    expect(panel).toHaveTextContent("Touched files")
    expect(panel).toHaveTextContent("src/in-workspace.ts")
    expect(panel).toHaveTextContent("+3 -1")
    expect(panel).toHaveTextContent("/tmp/outside.log")
    expect(screen.getByTestId("delegation-outside-workspace")).toHaveTextContent(
      "Outside workspace"
    )
    expect(screen.getByTestId("delegation-files-truncated")).toHaveTextContent(
      "List truncated to first 2 paths"
    )
  })

  it("omits the entire operational row when every segment is absent", () => {
    renderWithIntl(
      <DelegationCardChrome
        displaySecondary="just a title"
        elapsedMs={null}
        toolCallCount={null}
        editRollup={omitRollup}
        attentionRequest={null}
        runtimeStats={null}
        filesExpanded={false}
        onToggleFilesExpanded={() => {}}
      />
    )
    expect(
      screen.queryByTestId("delegation-operational")
    ).not.toBeInTheDocument()
  })
})
