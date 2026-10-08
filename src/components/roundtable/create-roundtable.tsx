"use client"

import { useTranslations } from "next-intl"

export function CreateRoundtable({
  mode,
  quotaError,
  confirmed,
  onConfirm,
  onStart,
  enabled = false,
  busy = false,
}: {
  mode: "parallel" | "serial"
  quotaError?: string
  confirmed: boolean
  onConfirm: () => void
  onStart?: () => void
  enabled?: boolean
  busy?: boolean
}) {
  const t = useTranslations("Roundtable")
  return (
    <section>
      <h1>{t("start")}</h1>
      <p>{mode === "parallel" ? t("parallel") : t("serial")}</p>
      {quotaError ? <p role="alert">{quotaError}</p> : null}
      <button type="button" onClick={onConfirm} disabled={busy}>
        {t("preflight")}
      </button>
      <button
        type="button"
        disabled={!confirmed || !enabled || busy || !onStart}
        onClick={onStart}
      >
        {t("start")}
      </button>
    </section>
  )
}
