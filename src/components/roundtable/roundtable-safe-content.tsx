"use client"

import { useTranslations } from "next-intl"

export function RoundtableSafeContent({
  text,
  rtl,
  preview = true,
}: {
  text: string
  rtl?: boolean
  preview?: boolean
}) {
  const t = useTranslations("Roundtable")
  const visible = text
    .replace(/[A-Za-z]:\\[^\s]+/g, "")
    .replace(/思考过程/g, "")
    .replace(/sk-[a-z0-9-]+/gi, "[redacted]")
  return (
    <article dir={rtl === undefined ? "auto" : rtl ? "rtl" : "ltr"}>
      <p className="whitespace-pre-wrap break-words">{visible}</p>
      {preview ? (
        <>
          <p>{t("preview")}</p>
          <p>{t("unknown")}</p>
        </>
      ) : null}
    </article>
  )
}
