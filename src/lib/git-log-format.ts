/** Display helpers shared by the git-log aux panel and the push workspace. */

export type GitLogTimeKey =
  | "time.monthsAgo"
  | "time.daysAgo"
  | "time.hoursAgo"
  | "time.minsAgo"
  | "time.justNow"

/**
 * Render a git commit date as a coarse "N units ago" string via the provided
 * translator. Falls back to the raw string when the date can't be parsed.
 */
export function formatRelativeTime(
  dateStr: string,
  t: (key: GitLogTimeKey, values?: { count: number }) => string
): string {
  const date = new Date(dateStr)
  if (Number.isNaN(date.getTime())) return dateStr

  const now = new Date()
  const diffMs = now.getTime() - date.getTime()
  const diffMin = Math.floor(diffMs / 60_000)
  const diffHour = Math.floor(diffMin / 60)
  const diffDay = Math.floor(diffHour / 24)

  if (diffDay > 30) {
    const diffMonth = Math.floor(diffDay / 30)
    return t("time.monthsAgo", { count: diffMonth })
  }
  if (diffDay > 0) return t("time.daysAgo", { count: diffDay })
  if (diffHour > 0) return t("time.hoursAgo", { count: diffHour })
  if (diffMin > 0) return t("time.minsAgo", { count: diffMin })
  return t("time.justNow", { count: 0 })
}

/** Parse a git date string, returning `null` when it is invalid. */
export function parseDate(dateStr: string): Date | null {
  const date = new Date(dateStr)
  return Number.isNaN(date.getTime()) ? null : date
}

/** Normalize a git status letter (A/D/R/…) to a change kind. */
export function mapFileStatus(
  status: string
): "added" | "modified" | "deleted" | "renamed" {
  switch (status.toUpperCase().charAt(0)) {
    case "A":
      return "added"
    case "D":
      return "deleted"
    case "R":
      return "renamed"
    default:
      return "modified"
  }
}
