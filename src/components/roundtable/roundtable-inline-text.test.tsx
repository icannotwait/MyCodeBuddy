import { render } from "@testing-library/react"
import { describe, expect, it } from "vitest"
import { plainInlineText, RoundtableInlineText } from "./roundtable-inline-text"

describe("roundtable inline title text", () => {
  it("renders code spans as code and keeps everything else as text", () => {
    const { container } = render(
      <h2>
        <RoundtableInlineText text="Use `HttpOnly` or `localStorage`? <b>no html</b> [x](javascript:alert(1))" />
      </h2>
    )
    const codes = [...container.querySelectorAll("code")].map(
      (node) => node.textContent
    )
    expect(codes).toEqual(["HttpOnly", "localStorage"])
    expect(container.querySelector("b")).toBeNull()
    expect(container.querySelector("a")).toBeNull()
    expect(container).toHaveTextContent("<b>no html</b>")
    expect(container).not.toHaveTextContent("`")
  })

  it("leaves unmatched backticks literal and strips spans for tooltips", () => {
    const { container } = render(<RoundtableInlineText text="a ` b" />)
    expect(container.querySelector("code")).toBeNull()
    expect(container).toHaveTextContent("a ` b")
    expect(plainInlineText("run `x` now")).toBe("run x now")
  })
})
