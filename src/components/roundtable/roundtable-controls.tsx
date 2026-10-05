"use client"

import { useTranslations } from "next-intl"

export function RoundtableControls({
  moderatorFailed,
  onPause,
  onRetry,
  onResume,
  onStop,
  busy = false,
}: {
  moderatorFailed: boolean
  onPause?: () => void
  onRetry?: () => void
  onResume?: () => void
  onStop?: () => void
  busy?: boolean
}) {
  const t = useTranslations("Roundtable")
  return (
    <section>
      <button type="button" disabled={busy || !onPause} onClick={onPause}>
        {t("pause")}
      </button>
      {moderatorFailed ? (
        <button type="button" disabled={busy || !onRetry} onClick={onRetry}>
          {t("retry")}
        </button>
      ) : null}
      {onResume ? (
        <button type="button" disabled={busy} onClick={onResume}>
          {t("resume")}
        </button>
      ) : null}
      {onStop ? (
        <button type="button" disabled={busy} onClick={onStop}>
          {t("stop")}
        </button>
      ) : null}
    </section>
  )
}
