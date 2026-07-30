import { render, screen } from "@testing-library/react"
import { describe, expect, it } from "vitest"

import { WorkflowStatusIcon } from "./workflow-status-icon"

describe("WorkflowStatusIcon", () => {
  it.each([
    ["completed", "completed"],
    ["current", "active"],
    ["running", "active"],
    ["blocked", "blocked"],
    ["failed", "blocked"],
    ["missing_summary", "blocked"],
    ["waiting_review", "waiting"],
    ["waiting_adjudication", "waiting"],
    ["canceled", "inactive"],
    ["pending", "inactive"],
    ["superseded", "inactive"],
    ["reserving", "reserving"],
    ["estimated", "estimated"],
    ["future_status", "inactive"],
  ])("maps %s to the %s visual bucket", (visualStatus, bucket) => {
    render(<WorkflowStatusIcon visualStatus={visualStatus} />)
    const icon = screen.getByTestId("workflow-status-icon")
    expect(icon).toHaveAttribute("data-visual-status", visualStatus)
    expect(icon).toHaveAttribute("data-status-bucket", bucket)
    expect(icon).toHaveAttribute("aria-hidden", "true")
  })

  it("limits pulse animation to motion-safe active visuals", () => {
    const { rerender } = render(<WorkflowStatusIcon visualStatus="running" />)
    expect(screen.getByTestId("workflow-status-icon")).toHaveClass(
      "motion-safe:animate-pulse"
    )
    rerender(<WorkflowStatusIcon visualStatus="completed" />)
    expect(screen.getByTestId("workflow-status-icon")).not.toHaveClass(
      "motion-safe:animate-pulse"
    )
  })

  it.each([
    ["completed", "bg-emerald-600", "svg.lucide-check"],
    ["blocked", "border-destructive", "svg.lucide-x"],
    ["waiting_review", "text-amber-600", "span > span"],
    ["reserving", "text-amber-600", "svg.lucide-clock-3"],
    ["estimated", "text-muted-foreground", "svg.lucide-circle-dashed"],
    ["future_status", "text-muted-foreground", "span.border-current"],
  ])(
    "renders %s with its frozen color and shape",
    (visualStatus, colorClass, shapeSelector) => {
      render(<WorkflowStatusIcon visualStatus={visualStatus} />)
      const icon = screen.getByTestId("workflow-status-icon")
      expect(icon).toHaveClass(colorClass)
      expect(icon.querySelector(shapeSelector)).not.toBeNull()
    }
  )
})
