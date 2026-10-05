import { render, screen } from "@testing-library/react"
import { describe, expect, it } from "vitest"

import { RoundtableControls } from "@/components/roundtable/roundtable-controls"
import { RoundtableDetail } from "@/components/roundtable/roundtable-detail"
import { RoundtableSafeContent } from "@/components/roundtable/roundtable-safe-content"
import { ROUNDTABLE_NAV_ENABLED } from "@/components/layout/sidebar"

describe("roundtable detail", () => {
  it("keeps order, pause copy, and unsafe text out of the page", () => {
    expect(ROUNDTABLE_NAV_ENABLED).toBe(false)
    render(
      <RoundtableDetail
        order={["成员甲", "成员乙", "主持人"]}
        partial="部分失败"
        waiting
        showSynthesis={false}
      />
    )
    expect(screen.getAllByRole("listitem").map((item) => item.textContent)).toEqual([
      "成员甲",
      "成员乙",
      "主持人",
    ])
    expect(screen.getByText("等待其余成员")).toBeTruthy()
    expect(screen.queryByRole("button", { name: "综合" })).toBeNull()
    render(<RoundtableControls moderatorFailed />)
    expect(screen.getByRole("button", { name: "立即暂停，保留已完成结果" })).toBeTruthy()
    expect(screen.getByRole("button", { name: "只重试主持" })).toBeTruthy()
    expect(screen.queryByRole("button", { name: "整轮重来" })).toBeNull()
    const { container } = render(
      <RoundtableSafeContent text="C:\\secret\\note sk-live-secret 思考过程" rtl />
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
