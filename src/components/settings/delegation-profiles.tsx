"use client"

import { useEffect, useMemo, useState } from "react"
import { Copy, Loader2, Plus, Trash2 } from "lucide-react"
import { useTranslations } from "next-intl"

import { Button } from "@/components/ui/button"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { describeAgentOptions } from "@/lib/api"
import type {
  AgentDelegationDefaults,
  DelegationProfile,
  AgentOptionsSnapshot,
} from "@/lib/types"
import { randomUUID } from "@/lib/utils"

import { DelegationOptionEditor } from "./delegation-agent-defaults"

interface DelegationProfilesPanelProps {
  value: DelegationProfile[]
  codeBuddyDefaults: AgentDelegationDefaults
  onChange: (profiles: DelegationProfile[]) => void
  disabled?: boolean
}

function nowMs(): number {
  return Date.now()
}

function nextCopyName(profiles: DelegationProfile[], source: string): string {
  const names = new Set(profiles.map((profile) => profile.name.toLowerCase()))
  let candidate = `${source} copy`
  let suffix = 2
  while (names.has(candidate.toLowerCase())) {
    candidate = `${source} copy ${suffix}`
    suffix += 1
  }
  return candidate
}

function nextNewName(profiles: DelegationProfile[], source: string): string {
  const exists = profiles.some(
    (profile) => profile.name.toLowerCase() === source.toLowerCase()
  )
  return exists ? nextCopyName(profiles, source) : source
}

export function DelegationProfilesPanel({
  value,
  codeBuddyDefaults,
  onChange,
  disabled,
}: DelegationProfilesPanelProps) {
  const t = useTranslations("AcpAgentSettings.multiAgent")
  const [selectedId, setSelectedId] = useState<string | null>(
    value[0]?.id ?? null
  )
  const [deleteId, setDeleteId] = useState<string | null>(null)
  const [snapshot, setSnapshot] = useState<AgentOptionsSnapshot | null>(null)
  const effectiveSelectedId = selectedId ?? value[0]?.id ?? null
  const selected =
    value.find((profile) => profile.id === effectiveSelectedId) ?? null
  const probing = selected !== null && snapshot === null

  useEffect(() => {
    if (!selected || snapshot) return
    let cancelled = false
    void describeAgentOptions("code_buddy")
      .then((options) => {
        if (!cancelled) setSnapshot(options)
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [selected, snapshot])

  const sorted = useMemo(
    () => [...value].sort((a, b) => a.created_at - b.created_at),
    [value]
  )

  const update = (id: string, patch: Partial<DelegationProfile>) => {
    const now = nowMs()
    onChange(
      value.map((profile) =>
        profile.id === id ? { ...profile, ...patch, updated_at: now } : profile
      )
    )
  }

  const create = () => {
    const now = nowMs()
    const id = randomUUID()
    const profile: DelegationProfile = {
      id,
      agent_type: "code_buddy",
      name: nextNewName(value, t("profileNewName")),
      mode_id: codeBuddyDefaults.mode_id,
      config_values: { ...codeBuddyDefaults.config_values },
      enabled: true,
      created_at: now,
      updated_at: now,
    }
    onChange([...value, profile])
    setSelectedId(id)
  }

  const duplicate = (profile: DelegationProfile) => {
    const now = nowMs()
    const copy: DelegationProfile = {
      ...profile,
      id: randomUUID(),
      name: nextCopyName(value, profile.name),
      config_values: { ...profile.config_values },
      created_at: now,
      updated_at: now,
    }
    onChange([...value, copy])
    setSelectedId(copy.id)
  }

  const remove = () => {
    if (!deleteId) return
    const id = deleteId
    const next = value.filter((profile) => profile.id !== id)
    onChange(next)
    setSelectedId(next[0]?.id ?? null)
    setDeleteId(null)
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-3">
        <p className="text-xs text-muted-foreground leading-5">
          {t("profilesDescription")}
        </p>
        <Button
          size="sm"
          variant="outline"
          onClick={create}
          disabled={disabled}
        >
          <Plus className="size-3.5" aria-hidden />
          {t("profileAdd")}
        </Button>
      </div>

      {sorted.length === 0 ? (
        <p className="border-y py-5 text-center text-xs text-muted-foreground">
          {t("profilesEmpty")}
        </p>
      ) : (
        <TooltipProvider>
          <div className="divide-y border-y">
            {sorted.map((profile) => (
              <div
                key={profile.id}
                className="flex min-h-12 items-center gap-2 py-2"
              >
                <button
                  type="button"
                  onClick={() => setSelectedId(profile.id)}
                  className="min-w-0 flex-1 text-left"
                >
                  <span className="block truncate text-sm font-medium">
                    CodeBuddy:{profile.name}
                  </span>
                  <span className="block truncate text-xs text-muted-foreground">
                    {profile.config_values.model ?? t("profileDefaultModel")}
                  </span>
                </button>
                <Switch
                  checked={profile.enabled}
                  onCheckedChange={(enabled) => update(profile.id, { enabled })}
                  disabled={disabled}
                  aria-label={t("profileEnabled")}
                />
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      onClick={() => duplicate(profile)}
                      disabled={disabled}
                      aria-label={t("profileDuplicate")}
                    >
                      <Copy className="size-3.5" />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>{t("profileDuplicate")}</TooltipContent>
                </Tooltip>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      onClick={() => setDeleteId(profile.id)}
                      disabled={disabled}
                      aria-label={t("profileDelete")}
                    >
                      <Trash2 className="size-3.5" />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>{t("profileDelete")}</TooltipContent>
                </Tooltip>
              </div>
            ))}
          </div>
        </TooltipProvider>
      )}

      {selected && (
        <div className="space-y-3 pt-2">
          <label className="block space-y-1 text-sm font-medium">
            <span>{t("profileName")}</span>
            <Input
              value={selected.name}
              maxLength={80}
              onChange={(event) =>
                update(selected.id, { name: event.target.value })
              }
              disabled={disabled}
            />
          </label>
          {probing && (
            <p className="flex items-center gap-2 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" />
              {t("probing")}
            </p>
          )}
          {snapshot && (
            <DelegationOptionEditor
              snapshot={snapshot}
              overrideModeId={selected.mode_id ?? null}
              overrideConfigValues={selected.config_values}
              onModeChange={(mode_id) =>
                update(selected.id, { mode_id: mode_id ?? undefined })
              }
              onConfigChange={(optionId, optionValue) => {
                const config_values = { ...selected.config_values }
                if (optionValue === null) delete config_values[optionId]
                else config_values[optionId] = optionValue
                update(selected.id, { config_values })
              }}
              disabled={disabled}
            />
          )}
        </div>
      )}

      <AlertDialog
        open={deleteId !== null}
        onOpenChange={(open) => !open && setDeleteId(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("profileDeleteTitle")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("profileDeleteMessage")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("profileDeleteCancel")}</AlertDialogCancel>
            <AlertDialogAction variant="destructive" onClick={remove}>
              {t("profileDelete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
