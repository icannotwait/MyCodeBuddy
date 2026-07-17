import { render, screen } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"
import type { DelegationRouteSnapshot } from "@/lib/types"

const h = vi.hoisted(() => ({
  delegationRoute: null as DelegationRouteSnapshot | null,
  isViewer: false,
  isDelegationChild: false,
  status: "connected" as string | null,
  reapplyConfig: vi.fn(async () => true),
}))

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string, params?: Record<string, string>) => {
    if (key === "safeFallback") return `fallback:${params?.reason ?? ""}`
    if (key.startsWith("reason.")) return key.slice("reason.".length)
    return key
  },
}))

vi.mock("@/hooks/use-connection", () => ({
  useConnection: () => ({
    delegationRoute: h.delegationRoute,
    isViewer: h.isViewer,
    isDelegationChild: h.isDelegationChild,
    status: h.status,
    reapplyConfig: h.reapplyConfig,
  }),
}))

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}))

import {
  DelegationRouteNotice,
  shouldShowRouteNotice,
} from "./delegation-route-notice"

describe("shouldShowRouteNotice", () => {
  it("hides normal Codeg and native routes", () => {
    expect(
      shouldShowRouteNotice({
        requested: "codeg",
        effective: "codeg",
        source: "global_default",
        managed: true,
        delegation_available: true,
      })
    ).toBe(false)
    expect(
      shouldShowRouteNotice({
        requested: "native",
        effective: "native",
        source: "session_override",
        managed: true,
        delegation_available: false,
      })
    ).toBe(false)
  })

  it("shows safe fallback and unavailable Codeg", () => {
    expect(
      shouldShowRouteNotice({
        requested: "codeg",
        effective: "native",
        source: "safe_fallback",
        managed: true,
        degraded_reason: "companion_binary_unavailable",
        delegation_available: false,
      })
    ).toBe(true)
    expect(
      shouldShowRouteNotice({
        requested: "codeg",
        effective: "codeg",
        source: "global_default",
        managed: true,
        delegation_available: false,
      })
    ).toBe(true)
  })
})

describe("DelegationRouteNotice", () => {
  beforeEach(() => {
    h.delegationRoute = null
    h.isViewer = false
    h.isDelegationChild = false
    h.status = "connected"
    h.reapplyConfig.mockClear()
  })

  it("renders nothing for a normal route", () => {
    h.delegationRoute = {
      requested: "codeg",
      effective: "codeg",
      source: "global_default",
      managed: true,
      delegation_available: true,
    }
    const { container } = render(<DelegationRouteNotice contextKey="tab-1" />)
    expect(container.firstChild).toBeNull()
  })

  it("renders a reconnect action for safe fallback", () => {
    h.delegationRoute = {
      requested: "codeg",
      effective: "native",
      source: "safe_fallback",
      managed: true,
      degraded_reason: "companion_binary_unavailable",
      delegation_available: false,
    }
    render(<DelegationRouteNotice contextKey="tab-1" />)
    expect(screen.getByText(/fallback:/i)).toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: /reconnect/i })
    ).toBeInTheDocument()
  })
})
