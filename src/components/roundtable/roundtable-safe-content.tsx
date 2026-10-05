"use client"

export function RoundtableSafeContent({
  text,
  rtl,
}: {
  text: string
  rtl?: boolean
}) {
  const visible = text
    .replace(/[A-Za-z]:\\[^\s]+/g, "")
    .replace(/思考过程/g, "")
    .replace(/sk-[a-z0-9-]+/gi, "[redacted]")
  return (
    <article dir={rtl ? "rtl" : "ltr"}>
      <p>{visible}</p>
      <p>生成中，不是最终结果</p>
      <p>unknown</p>
    </article>
  )
}
