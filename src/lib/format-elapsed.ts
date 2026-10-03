/**
 * Resolves the localized elapsed-time unit keys
 * (`elapsedHours` / `elapsedMinutes` / `elapsedSeconds`). A
 * `useTranslations("Folder.chat.liveTurnStats")` instance satisfies this.
 *
 * Typed narrowly (rather than `ReturnType<typeof useTranslations>`) on purpose:
 * invoking next-intl's full translator type with a values argument trips its
 * recursive message-key conditionals ("type instantiation is excessively deep").
 */
export type ElapsedUnitTranslator = (
  key: "elapsedHours" | "elapsedMinutes" | "elapsedSeconds",
  values: { value: number }
) => string

/**
 * Formats a duration (in milliseconds) as the localized
 * `Xh Ym Zs` / `Ym Zs` / `Zs` label used by the live turn timer shown above the
 * composer. Sharing this keeps the per-turn execution-time tooltip identical to
 * the live timer. Units are floored to whole numbers, matching the once-per-
 * second live tick (sub-second durations render as `0s`).
 */
export function formatElapsedLabel(
  ms: number,
  t: ElapsedUnitTranslator
): string {
  const total = Number.isFinite(ms) ? Math.max(0, ms) : 0
  const hours = Math.floor(total / 3_600_000)
  const minutes = Math.floor((total % 3_600_000) / 60_000)
  const seconds = Math.floor((total % 60_000) / 1_000)

  if (hours > 0) {
    return `${t("elapsedHours", { value: hours })} ${t("elapsedMinutes", { value: minutes })} ${t("elapsedSeconds", { value: seconds })}`
  }
  if (minutes > 0) {
    return `${t("elapsedMinutes", { value: minutes })} ${t("elapsedSeconds", { value: seconds })}`
  }
  return t("elapsedSeconds", { value: seconds })
}

/** Compact duration for the workflow floating card. */
export function formatCompactElapsed(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "0s"

  if (ms >= 3_600_000) {
    const totalMinutes = Math.ceil(ms / 60_000)
    const hours = Math.floor(totalMinutes / 60)
    const minutes = totalMinutes % 60
    return `${hours}h${minutes ? `${minutes}m` : ""}`
  }

  const totalSeconds = Math.floor(ms / 1_000)
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  return minutes ? `${minutes}m${seconds ? `${seconds}s` : ""}` : `${seconds}s`
}
