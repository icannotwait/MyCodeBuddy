"use client"

/**
 * Tool-execution watchdog settings — global kill switch + warning/grace
 * durations (clamped 60..=3600s). Persisted via
 * `acp_get/set_tool_watchdog_settings`. Mounted under Settings > General as
 * its own section, outside Delegation (which keeps the 300s soft-watchdog).
 */

import { useCallback, useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Loader2, Timer } from "lucide-react"
import { toast } from "sonner"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import { getToolWatchdogSettings, setToolWatchdogSettings } from "@/lib/api"
import type { ToolWatchdogSettings } from "@/lib/types"
import { toErrorMessage } from "@/lib/app-error"

export const TOOL_WATCHDOG_DURATION_MIN = 60
export const TOOL_WATCHDOG_DURATION_MAX = 3_600
export const TOOL_WATCHDOG_DEFAULT_WARNING = 600
export const TOOL_WATCHDOG_DEFAULT_GRACE = 600

export function clampToolWatchdogDuration(n: number): number {
  if (!Number.isFinite(n)) return TOOL_WATCHDOG_DURATION_MIN
  return Math.min(
    TOOL_WATCHDOG_DURATION_MAX,
    Math.max(TOOL_WATCHDOG_DURATION_MIN, Math.trunc(n))
  )
}

export function ToolWatchdogSettingsSection() {
  const t = useTranslations("ToolWatchdogSettings")
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [enabled, setEnabled] = useState(false)
  const [warningAfter, setWarningAfter] = useState(
    TOOL_WATCHDOG_DEFAULT_WARNING
  )
  const [graceSeconds, setGraceSeconds] = useState(TOOL_WATCHDOG_DEFAULT_GRACE)
  const [loadError, setLoadError] = useState<string | null>(null)
  // When the user has local edits, later background reloads must not clobber
  // the dirty form (e.g. remount races). Save clears dirty by applying server.
  const dirtyRef = useRef(false)

  const applySettings = useCallback((s: ToolWatchdogSettings) => {
    setEnabled(s.enabled)
    setWarningAfter(s.warning_after_seconds)
    setGraceSeconds(s.grace_seconds)
    dirtyRef.current = false
  }, [])

  useEffect(() => {
    let cancelled = false
    void getToolWatchdogSettings()
      .then((s) => {
        if (cancelled) return
        if (dirtyRef.current) return
        applySettings(s)
        setLoadError(null)
      })
      .catch((err: unknown) => {
        if (cancelled) return
        setLoadError(toErrorMessage(err))
      })
      .finally(() => {
        if (cancelled) return
        setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [applySettings])

  const markDirty = useCallback(() => {
    dirtyRef.current = true
  }, [])

  const save = useCallback(async () => {
    const payload: ToolWatchdogSettings = {
      enabled,
      warning_after_seconds: clampToolWatchdogDuration(warningAfter),
      grace_seconds: clampToolWatchdogDuration(graceSeconds),
    }
    setSaving(true)
    try {
      const applied = await setToolWatchdogSettings(payload)
      applySettings(applied)
      toast.success(t("saved"))
    } catch (err: unknown) {
      toast.error(t("saveFailed"), { description: toErrorMessage(err) })
    } finally {
      setSaving(false)
    }
  }, [enabled, warningAfter, graceSeconds, applySettings, t])

  return (
    <section
      className="rounded-xl border bg-card p-4 space-y-4"
      data-testid="tool-watchdog-settings"
    >
      <div className="flex items-center gap-2">
        <Timer className="h-4 w-4 text-muted-foreground" aria-hidden />
        <h2 className="text-sm font-semibold break-words">{t("title")}</h2>
      </div>
      <p className="text-xs text-muted-foreground leading-5 break-words">
        {t("description")}
      </p>

      {loadError && (
        <p className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive break-words">
          {t("loadFailed", { detail: loadError })}
        </p>
      )}

      <div className="flex items-center justify-between gap-3">
        <div className="space-y-1 min-w-0">
          <label
            htmlFor="tool-watchdog-enabled"
            className="text-sm font-medium"
          >
            {t("enable")}
          </label>
          <p className="text-xs text-muted-foreground break-words">
            {t("enableHint")}
          </p>
        </div>
        <Switch
          id="tool-watchdog-enabled"
          checked={enabled}
          onCheckedChange={(next) => {
            markDirty()
            setEnabled(next)
          }}
          disabled={loading}
          className="shrink-0"
        />
      </div>

      <div className="flex items-center justify-between gap-3">
        <div className="space-y-1 min-w-0">
          <label
            htmlFor="tool-watchdog-warning"
            className="text-sm font-medium"
          >
            {t("warningAfter")}
          </label>
          <p className="text-xs text-muted-foreground break-words">
            {t("warningAfterHint", {
              min: TOOL_WATCHDOG_DURATION_MIN,
              max: TOOL_WATCHDOG_DURATION_MAX,
            })}
          </p>
        </div>
        <Input
          id="tool-watchdog-warning"
          type="number"
          min={TOOL_WATCHDOG_DURATION_MIN}
          max={TOOL_WATCHDOG_DURATION_MAX}
          value={warningAfter}
          onChange={(e) => {
            markDirty()
            setWarningAfter(Number(e.target.value))
          }}
          onBlur={() => setWarningAfter((v) => clampToolWatchdogDuration(v))}
          disabled={loading || !enabled}
          className="w-28 shrink-0"
        />
      </div>

      <div className="flex items-center justify-between gap-3">
        <div className="space-y-1 min-w-0">
          <label htmlFor="tool-watchdog-grace" className="text-sm font-medium">
            {t("graceSeconds")}
          </label>
          <p className="text-xs text-muted-foreground break-words">
            {t("graceSecondsHint", {
              min: TOOL_WATCHDOG_DURATION_MIN,
              max: TOOL_WATCHDOG_DURATION_MAX,
            })}
          </p>
        </div>
        <Input
          id="tool-watchdog-grace"
          type="number"
          min={TOOL_WATCHDOG_DURATION_MIN}
          max={TOOL_WATCHDOG_DURATION_MAX}
          value={graceSeconds}
          onChange={(e) => {
            markDirty()
            setGraceSeconds(Number(e.target.value))
          }}
          onBlur={() => setGraceSeconds((v) => clampToolWatchdogDuration(v))}
          disabled={loading || !enabled}
          className="w-28 shrink-0"
        />
      </div>

      <div className="flex justify-end pt-2">
        <Button onClick={save} disabled={loading || saving} size="sm">
          {saving ? (
            <>
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
              {t("saving")}
            </>
          ) : (
            t("save")
          )}
        </Button>
      </div>
    </section>
  )
}
