"use client"

/**
 * Multi-agent delegation settings panel. Owns the three knobs persisted by
 * `set_delegation_settings_core` on the Rust side:
 *
 *   * `enabled` — feature kill switch
 *   * `depth_limit` — bounds chain depth (1..=8)
 *   * `default_timeout_seconds` — broker fallback when LLM omits it (30..=3600)
 *
 * Mounted under `/settings/general` next to the terminal and rendering
 * sections, because delegation is a global feature — not per-agent — and
 * doesn't belong inside the 7,800-line `acp-agent-settings.tsx` that
 * powers `/settings/agents`.
 */

import { useCallback, useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { Bubbles, Loader2 } from "lucide-react"
import { toast } from "sonner"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import {
  type DelegationSettings,
  getDelegationSettings,
  setDelegationSettings,
} from "@/lib/api"

const DEPTH_MIN = 1
const DEPTH_MAX = 8
const TIMEOUT_MIN = 30
const TIMEOUT_MAX = 3600

function clamp(n: number, lo: number, hi: number): number {
  if (!Number.isFinite(n)) return lo
  return Math.min(hi, Math.max(lo, Math.trunc(n)))
}

export function DelegationSettingsSection() {
  const t = useTranslations("AcpAgentSettings.multiAgent")
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [enabled, setEnabled] = useState(true)
  const [depth, setDepth] = useState<number>(2)
  const [timeout, setTimeoutValue] = useState<number>(600)
  const [loadError, setLoadError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    void getDelegationSettings()
      .then((s) => {
        if (cancelled) return
        setEnabled(s.enabled)
        setDepth(s.depth_limit)
        setTimeoutValue(s.default_timeout_seconds)
        setLoadError(null)
      })
      .catch((err: unknown) => {
        if (cancelled) return
        setLoadError(err instanceof Error ? err.message : String(err))
      })
      .finally(() => {
        if (cancelled) return
        setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [])

  const save = useCallback(async () => {
    const payload: DelegationSettings = {
      enabled,
      depth_limit: clamp(depth, DEPTH_MIN, DEPTH_MAX),
      default_timeout_seconds: clamp(timeout, TIMEOUT_MIN, TIMEOUT_MAX),
    }
    setSaving(true)
    try {
      const applied = await setDelegationSettings(payload)
      // Mirror any server-side clamps back into the UI so the inputs reflect
      // what was actually persisted.
      setEnabled(applied.enabled)
      setDepth(applied.depth_limit)
      setTimeoutValue(applied.default_timeout_seconds)
      toast.success(t("saved"))
    } catch (err: unknown) {
      toast.error(t("saveFailed"), {
        description: err instanceof Error ? err.message : String(err),
      })
    } finally {
      setSaving(false)
    }
  }, [enabled, depth, timeout, t])

  return (
    <section className="rounded-xl border bg-card p-4 space-y-4">
      <div className="flex items-center gap-2">
        <Bubbles className="h-4 w-4 text-muted-foreground" aria-hidden />
        <h2 className="text-sm font-semibold">{t("title")}</h2>
      </div>
      <p className="text-xs text-muted-foreground leading-5">
        {t("description")}
      </p>

      {loadError && (
        <p className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">
          {t("loadFailed", { detail: loadError })}
        </p>
      )}

      <div className="space-y-4">
        <div className="flex items-center justify-between gap-3">
          <div className="space-y-1">
            <label htmlFor="delegation-enabled" className="text-sm font-medium">
              {t("enable")}
            </label>
            <p className="text-xs text-muted-foreground">{t("enableHint")}</p>
          </div>
          <Switch
            id="delegation-enabled"
            checked={enabled}
            onCheckedChange={setEnabled}
            disabled={loading}
          />
        </div>

        <div className="flex items-center justify-between gap-3">
          <div className="space-y-1">
            <label htmlFor="delegation-depth" className="text-sm font-medium">
              {t("depthLimit")}
            </label>
            <p className="text-xs text-muted-foreground">
              {t("depthHint", { min: DEPTH_MIN, max: DEPTH_MAX })}
            </p>
          </div>
          <Input
            id="delegation-depth"
            type="number"
            min={DEPTH_MIN}
            max={DEPTH_MAX}
            value={depth}
            onChange={(e) => setDepth(Number(e.target.value))}
            disabled={loading || !enabled}
            className="w-24"
          />
        </div>

        <div className="flex items-center justify-between gap-3">
          <div className="space-y-1">
            <label htmlFor="delegation-timeout" className="text-sm font-medium">
              {t("timeoutSeconds")}
            </label>
            <p className="text-xs text-muted-foreground">
              {t("timeoutHint", { min: TIMEOUT_MIN, max: TIMEOUT_MAX })}
            </p>
          </div>
          <Input
            id="delegation-timeout"
            type="number"
            min={TIMEOUT_MIN}
            max={TIMEOUT_MAX}
            value={timeout}
            onChange={(e) => setTimeoutValue(Number(e.target.value))}
            disabled={loading || !enabled}
            className="w-32"
          />
        </div>
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
