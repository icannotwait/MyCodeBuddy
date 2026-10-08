"use client"

import { useTranslations } from "next-intl"
import { redactRoundtableText } from "@/lib/roundtable/redact"

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
  const visible = redactRoundtableText(text)
  return (
    <article
      className="min-w-0"
      dir={rtl === undefined ? "auto" : rtl ? "rtl" : "ltr"}
    >
      <p className="whitespace-pre-wrap [overflow-wrap:anywhere]">{visible}</p>
      {preview ? (
        <>
          <p>{t("preview")}</p>
          <p>{t("unknown")}</p>
        </>
      ) : null}
    </article>
  )
}
