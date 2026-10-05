import { fireEvent, render, screen } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"

import { RoundtableControls } from "@/components/roundtable/roundtable-controls"
import { RoundtableDetail } from "@/components/roundtable/roundtable-detail"
import { RoundtableSafeContent } from "@/components/roundtable/roundtable-safe-content"
import { ROUNDTABLE_NAV_ENABLED } from "@/components/layout/sidebar"
import messages from "@/i18n/messages/zh-CN.json"
vi.mock("next-intl", () => ({
  useTranslations: () => (key: keyof typeof messages.Roundtable) =>
    messages.Roundtable[key],
}))

describe("roundtable detail", () => {
  it("dispatches pause and moderator retry callbacks", () => {
    const pause = vi.fn()
    const retry = vi.fn()
    render(
      <RoundtableControls moderatorFailed onPause={pause} onRetry={retry} />
    )
    fireEvent.click(
      screen.getByRole("button", { name: "立即暂停，保留已完成结果" })
    )
    fireEvent.click(screen.getByRole("button", { name: "只重试主持" }))
    expect(pause).toHaveBeenCalledOnce()
    expect(retry).toHaveBeenCalledOnce()
  })
  it("keeps order, pause copy, and unsafe text out of the page", () => {
    expect(ROUNDTABLE_NAV_ENABLED).toBe(true)
    render(
      <RoundtableDetail
        order={["成员甲", "成员乙", "主持人"]}
        partial="部分失败"
        waiting
        showSynthesis={false}
      />
    )
    expect(
      screen.getAllByRole("listitem").map((item) => item.textContent)
    ).toEqual(["成员甲", "成员乙", "主持人"])
    expect(screen.getByText("等待其余成员")).toBeTruthy()
    expect(screen.queryByRole("button", { name: "综合" })).toBeNull()
    render(<RoundtableControls moderatorFailed />)
    expect(
      screen.getByRole("button", { name: "立即暂停，保留已完成结果" })
    ).toBeTruthy()
    expect(screen.getByRole("button", { name: "只重试主持" })).toBeTruthy()
    expect(screen.queryByRole("button", { name: "整轮重来" })).toBeNull()
    const { container } = render(
      <RoundtableSafeContent
        text="C:\\secret\\note sk-live-secret 思考过程"
        rtl
      />
    )
    const article = container.querySelector("article")
    expect(article?.getAttribute("dir")).toBe("rtl")
    expect(article?.textContent).not.toContain("sk-live-secret")
    expect(article?.textContent).not.toContain("C:\\secret")
    expect(article?.textContent).not.toContain("思考过程")
    expect(screen.getByText("unknown")).toBeTruthy()
    expect(screen.getByText("生成中，不是最终结果")).toBeTruthy()
  })
})
