"use client"

/**
 * Conversation-experience settings: automatic title agent selection and
 * reference-search result limit.
 * Mounted under General settings before multi-agent delegation.
 */

import { useCallback, useEffect, useMemo, useState } from "react"
import { useTranslations } from "next-intl"
import { Loader2, Sparkles } from "lucide-react"
import { toast } from "sonner"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { useAcpAgents } from "@/hooks/use-acp-agents"
import { toErrorMessage } from "@/lib/app-error"
import type { AgentType } from "@/lib/types"
import {
  useConversationExperienceBootstrap,
  useConversationExperienceStore,
} from "@/stores/conversation-experience-store"

const OFF_VALUE = "__off__"
const MIN_REFERENCE_SEARCH_LIMIT = 10
const MAX_REFERENCE_SEARCH_LIMIT = 500

function clampReferenceSearchLimit(raw: number): number {
  if (!Number.isFinite(raw)) return MIN_REFERENCE_SEARCH_LIMIT
  const n = Math.trunc(raw)
  return Math.min(
    MAX_REFERENCE_SEARCH_LIMIT,
    Math.max(MIN_REFERENCE_SEARCH_LIMIT, n)
  )
}

export function ConversationExperienceSettingsSection() {
  const t = useTranslations("GeneralSettings")
  useConversationExperienceBootstrap()
  const settings = useConversationExperienceStore((s) => s.settings)
  const loading = useConversationExperienceStore((s) => s.loading)
  const setAutoTitleAgent = useConversationExperienceStore(
    (s) => s.setAutoTitleAgent
  )
  const setReferenceSearchLimit = useConversationExperienceStore(
    (s) => s.setReferenceSearchLimit
  )
  const { agents } = useAcpAgents()
  const [saving, setSaving] = useState(false)
  const [savingLimit, setSavingLimit] = useState(false)
  const [limitDraft, setLimitDraft] = useState(
    String(settings?.reference_search_limit ?? 50)
  )
  const [limitRevision, setLimitRevision] = useState(settings?.revision ?? null)

  // Adopt server document into the limit field only when revision advances
  // (same gating as the store) so in-progress edits are not clobbered.
  useEffect(() => {
    if (settings == null) return
    if (limitRevision != null && settings.revision <= limitRevision) return
    setLimitDraft(String(settings.reference_search_limit))
    setLimitRevision(settings.revision)
  }, [settings, limitRevision])

  const savedAgent = settings?.auto_title_agent ?? null

  const choices = useMemo(() => {
    const enabledAvailable = agents.filter((a) => a.enabled && a.available)
    const savedUnavailable =
      savedAgent != null &&
      !enabledAvailable.some((a) => a.agent_type === savedAgent)
        ? (agents.find((a) => a.agent_type === savedAgent) ?? {
            agent_type: savedAgent,
            name: savedAgent,
            enabled: false,
            available: false,
          })
        : null
    return { enabledAvailable, savedUnavailable }
  }, [agents, savedAgent])

  const selectValue = savedAgent ?? OFF_VALUE

  const onChange = useCallback(
    async (value: string) => {
      const next: AgentType | null =
        value === OFF_VALUE ? null : (value as AgentType)
      setSaving(true)
      try {
        await setAutoTitleAgent(next)
      } catch (err: unknown) {
        toast.error(t("autoTitleSaveFailed", { message: toErrorMessage(err) }))
      } finally {
        setSaving(false)
      }
    },
    [setAutoTitleAgent, t]
  )

  const applyLimitDraftClamp = useCallback(() => {
    const clamped = clampReferenceSearchLimit(Number(limitDraft))
    setLimitDraft(String(clamped))
    return clamped
  }, [limitDraft])

  const onSaveLimit = useCallback(async () => {
    const clamped = applyLimitDraftClamp()
    setSavingLimit(true)
    try {
      const saved = await setReferenceSearchLimit(clamped)
      setLimitDraft(String(saved.reference_search_limit))
      setLimitRevision(saved.revision)
    } catch (err: unknown) {
      toast.error(
        t("referenceSearchLimitSaveFailed", {
          message: toErrorMessage(err),
        })
      )
    } finally {
      setSavingLimit(false)
    }
  }, [applyLimitDraftClamp, setReferenceSearchLimit, t])

  return (
    <section className="rounded-xl border bg-card p-4 space-y-4">
      <div className="flex items-center gap-2">
        <Sparkles className="h-4 w-4 text-muted-foreground" aria-hidden />
        <h2 className="text-sm font-semibold">
          {t("conversationExperienceTitle")}
        </h2>
      </div>
      <p className="text-xs text-muted-foreground leading-5">
        {t("conversationExperienceDescription")}
      </p>

      {loading && !settings ? (
        <p className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 className="h-3.5 w-3.5 animate-spin" />
          {t("autoTitleLoading")}
        </p>
      ) : (
        <div className="space-y-4">
          <div className="space-y-2">
            <div className="flex items-center justify-between gap-3">
              <div className="space-y-1 min-w-0">
                <label
                  htmlFor="auto-title-agent"
                  className="text-sm font-medium"
                >
                  {t("autoTitleAgent")}
                </label>
              </div>
              <Select
                value={selectValue}
                onValueChange={onChange}
                disabled={saving || loading}
              >
                <SelectTrigger
                  id="auto-title-agent"
                  className="w-[220px] shrink-0"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={OFF_VALUE}>{t("autoTitleOff")}</SelectItem>
                  {choices.enabledAvailable.map((agent) => (
                    <SelectItem key={agent.agent_type} value={agent.agent_type}>
                      {agent.name}
                    </SelectItem>
                  ))}
                  {choices.savedUnavailable && (
                    <SelectItem
                      value={choices.savedUnavailable.agent_type}
                      disabled
                    >
                      {t("autoTitleUnavailable", {
                        agent: choices.savedUnavailable.name,
                      })}
                    </SelectItem>
                  )}
                </SelectContent>
              </Select>
            </div>
            <p
              className="text-xs text-muted-foreground leading-5"
              data-testid="translate-provider-disclosure"
            >
              {t("translateProviderDisclosure")}
            </p>
          </div>

          <div className="flex items-start justify-between gap-3">
            <div className="space-y-1 min-w-0">
              <label
                htmlFor="reference-search-limit"
                className="text-sm font-medium"
              >
                {t("referenceSearchLimit")}
              </label>
              <p className="text-xs text-muted-foreground leading-5">
                {t("referenceSearchLimitHint")}
              </p>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <Input
                id="reference-search-limit"
                type="number"
                min={MIN_REFERENCE_SEARCH_LIMIT}
                max={MAX_REFERENCE_SEARCH_LIMIT}
                step={1}
                inputMode="numeric"
                className="w-[7rem]"
                value={limitDraft}
                disabled={savingLimit || loading}
                onChange={(e) => setLimitDraft(e.target.value)}
                onBlur={() => {
                  applyLimitDraftClamp()
                }}
                aria-label={t("referenceSearchLimit")}
              />
              <Button
                type="button"
                size="sm"
                disabled={savingLimit || loading}
                onClick={() => {
                  void onSaveLimit()
                }}
              >
                {savingLimit ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  t("referenceSearchLimitSave")
                )}
              </Button>
            </div>
          </div>
        </div>
      )}
    </section>
  )
}
