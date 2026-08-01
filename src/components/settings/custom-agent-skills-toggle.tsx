"use client"

/**
 * Skills declaration card on a custom agent's settings page.
 *
 * codeg cannot detect where an arbitrary ACP agent loads skills from, so the
 * user declares it: the agent reads the shared `.agents/skills` store, a
 * dedicated directory of its own, or both. The declarations live on the
 * stored definition (`skills_shared_store` / `skills_dir`), which is why every
 * change re-saves the whole definition rather than writing a separate
 * preference: hydration re-publishes the registry and every skills surface
 * (custom skills, experts, office, science) follows from the same gate.
 *
 * Registry-added agents default to all-off — this card is their only way in,
 * the add dialog's fields only cover the manual form.
 */

import { useCallback, useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Loader2 } from "lucide-react"
import { toast } from "sonner"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import {
  acpListCustomAgents,
  acpSaveCustomAgent,
  type CustomAgentInfo,
  type CustomDistributionKind,
} from "@/lib/api"

interface CustomAgentSkillsToggleProps {
  registryId: string
}

export function CustomAgentSkillsToggle({
  registryId,
}: CustomAgentSkillsToggleProps) {
  const t = useTranslations("AcpAgentSettings")
  const [info, setInfo] = useState<CustomAgentInfo | null>(null)
  const [saving, setSaving] = useState(false)
  // The directory input is a draft: it only persists on an explicit save, so
  // half-typed paths never round-trip through the backend's absolute-path
  // check.
  const [dirDraft, setDirDraft] = useState("")
  const mountedRef = useRef(true)

  useEffect(() => {
    mountedRef.current = true
    let cancelled = false
    acpListCustomAgents()
      .then((list) => {
        if (cancelled) return
        const found = list.find((a) => a.registryId === registryId) ?? null
        setInfo(found)
        setDirDraft(found?.skillsDir ?? "")
      })
      .catch(() => {
        // Leave the card hidden; the surrounding page already surfaces
        // list/load errors.
      })
    return () => {
      cancelled = true
      mountedRef.current = false
    }
  }, [registryId])

  // One save path for both declarations, always sending the full definition —
  // a save that dropped either field would silently erase the other choice.
  const saveDeclarations = useCallback(
    async (
      current: CustomAgentInfo,
      next: { skillsSharedStore: boolean; skillsDir: string | null }
    ) => {
      await acpSaveCustomAgent({
        registryId: current.registryId,
        name: current.name,
        description: current.description,
        version: current.version,
        distributionKind: current.distributionKind as CustomDistributionKind,
        spec: current.spec,
        iconUrl: current.iconUrl,
        // Whole-definition re-save: every field must ride along, or this
        // card would silently reset it (provenance and the version probe
        // included).
        source: current.source,
        versionProbe: current.versionProbe,
        ...next,
      })
    },
    []
  )

  const handleToggle = useCallback(
    async (next: boolean) => {
      if (!info) return
      setSaving(true)
      // Optimistic: the switch reflects the target state while the save runs
      // and snaps back on failure. The directory rides along unchanged, from
      // its saved value — not the draft — so flipping the switch can never
      // commit a path the user was still typing.
      setInfo({ ...info, skillsSharedStore: next })
      try {
        await saveDeclarations(info, {
          skillsSharedStore: next,
          skillsDir: info.skillsDir,
        })
      } catch (err) {
        if (mountedRef.current) {
          setInfo({ ...info, skillsSharedStore: !next })
        }
        toast.error(err instanceof Error ? err.message : String(err))
      } finally {
        if (mountedRef.current) setSaving(false)
      }
    },
    [info, saveDeclarations]
  )

  const handleSaveDir = useCallback(async () => {
    if (!info) return
    const next = dirDraft.trim() || null
    setSaving(true)
    try {
      await saveDeclarations(info, {
        skillsSharedStore: info.skillsSharedStore,
        skillsDir: next,
      })
      // The backend may have normalized the path (`~` expansion); re-read so
      // the card shows what was actually stored.
      const list = await acpListCustomAgents()
      if (mountedRef.current) {
        const found = list.find((a) => a.registryId === info.registryId) ?? info
        setInfo(found)
        setDirDraft(found.skillsDir ?? "")
      }
    } catch (err) {
      // Keep the draft as typed so the user can correct it in place.
      toast.error(err instanceof Error ? err.message : String(err))
    } finally {
      if (mountedRef.current) setSaving(false)
    }
  }, [dirDraft, info, saveDeclarations])

  if (!info) return null

  const dirDirty = dirDraft.trim() !== (info.skillsDir ?? "")

  return (
    <div className="space-y-3 rounded-md border bg-muted/10 p-3">
      <div className="flex items-start justify-between gap-3">
        <div>
          <label className="text-xs font-medium">
            {t("customAgentSkillsLabel")}
          </label>
          <p className="mt-1 text-[11px] text-muted-foreground">
            {t("customAgentSkillsHint")}
          </p>
        </div>
        <Switch
          checked={info.skillsSharedStore}
          disabled={saving}
          onCheckedChange={(v) => void handleToggle(v)}
        />
      </div>

      <div className="space-y-1">
        <label
          className="text-xs font-medium"
          htmlFor="custom-agent-skills-dir"
        >
          {t("customAgentSkillsDirLabel")}
        </label>
        <div className="flex items-center gap-2">
          <Input
            id="custom-agent-skills-dir"
            value={dirDraft}
            disabled={saving}
            onChange={(e) => setDirDraft(e.target.value)}
            placeholder="~/.my-agent/skills"
            className="h-8 text-xs font-mono"
          />
          {dirDirty && (
            <Button
              size="sm"
              variant="secondary"
              className="h-8 text-xs shrink-0"
              disabled={saving}
              onClick={() => void handleSaveDir()}
            >
              {saving && <Loader2 className="h-3.5 w-3.5 mr-1 animate-spin" />}
              {t("customAgentSaveChanges")}
            </Button>
          )}
        </div>
        <p className="text-[11px] text-muted-foreground">
          {t("customAgentSkillsDirHint")}
        </p>
      </div>
    </div>
  )
}
