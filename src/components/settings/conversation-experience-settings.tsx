"use client"

/**
 * Conversation-experience settings: automatic title HTTP API config, document
 * translation agent, and reference-search result limit.
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
import type { AgentType, ApiKeyUpdate } from "@/lib/types"
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

function titleConfigComplete(
  url: string,
  keySet: boolean,
  model: string
): boolean {
  return url.trim().length > 0 && keySet && model.trim().length > 0
}

export function ConversationExperienceSettingsSection() {
  const t = useTranslations("GeneralSettings")
  useConversationExperienceBootstrap()
  const settings = useConversationExperienceStore((s) => s.settings)
  const loading = useConversationExperienceStore((s) => s.loading)
  const setAutoTitleApiConfig = useConversationExperienceStore(
    (s) => s.setAutoTitleApiConfig
  )
  const setDocumentTranslateAgent = useConversationExperienceStore(
    (s) => s.setDocumentTranslateAgent
  )
  const setReferenceSearchLimit = useConversationExperienceStore(
    (s) => s.setReferenceSearchLimit
  )
  const { agents } = useAcpAgents()

  const [savingTitle, setSavingTitle] = useState(false)
  const [savingTranslate, setSavingTranslate] = useState(false)
  const [savingLimit, setSavingLimit] = useState(false)

  const [urlDraft, setUrlDraft] = useState(settings?.auto_title_api_url ?? "")
  const [modelDraft, setModelDraft] = useState(settings?.auto_title_model ?? "")
  const [keyDraft, setKeyDraft] = useState("")
  const [keyCleared, setKeyCleared] = useState(false)
  const [titleRevision, setTitleRevision] = useState(settings?.revision ?? null)
  // Last adopted/synced title values. Dirty detection compares drafts to this
  // baseline — never to the incoming snapshot — so a clean form can adopt
  // external url/model/key_set changes when revision advances.
  const [titleBaseline, setTitleBaseline] = useState<{
    url: string
    model: string
  } | null>(() =>
    settings != null
      ? {
          url: settings.auto_title_api_url,
          model: settings.auto_title_model,
        }
      : null
  )

  const [limitDraft, setLimitDraft] = useState(
    String(settings?.reference_search_limit ?? 50)
  )
  const [limitRevision, setLimitRevision] = useState(settings?.revision ?? null)

  // Adopt server title fields when revision advances only if the local title
  // form is clean (drafts match last-synced baseline, no key draft/Clear).
  // Unrelated settings saves bump the shared revision and must not discard
  // pending URL/model/key edits or Clear-key intent. After a successful title
  // Save, onSaveTitle adopts and refreshes the baseline.
  useEffect(() => {
    if (settings == null) return
    if (titleRevision != null && settings.revision <= titleRevision) return

    const hasSynced = titleBaseline != null
    const titleFormDirty =
      hasSynced &&
      (keyCleared ||
        keyDraft.trim().length > 0 ||
        urlDraft !== titleBaseline.url ||
        modelDraft !== titleBaseline.model)

    if (!titleFormDirty) {
      setUrlDraft(settings.auto_title_api_url)
      setModelDraft(settings.auto_title_model)
      setKeyDraft("")
      setKeyCleared(false)
      setTitleBaseline({
        url: settings.auto_title_api_url,
        model: settings.auto_title_model,
      })
    }
    setTitleRevision(settings.revision)
  }, [
    settings,
    titleRevision,
    titleBaseline,
    keyCleared,
    keyDraft,
    urlDraft,
    modelDraft,
  ])

  useEffect(() => {
    if (settings == null) return
    if (limitRevision != null && settings.revision <= limitRevision) return
    setLimitDraft(String(settings.reference_search_limit))
    setLimitRevision(settings.revision)
  }, [settings, limitRevision])

  const keySetEffective =
    !keyCleared &&
    ((settings?.auto_title_api_key_set ?? false) || keyDraft.trim().length > 0)
  const barrier = settings?.auto_title_config_barrier ?? false
  const complete = titleConfigComplete(urlDraft, keySetEffective, modelDraft)

  const statusLabel = barrier
    ? t("autoTitleStatusBarrier")
    : complete
      ? t("autoTitleStatusEnabled")
      : t("autoTitleStatusIncomplete")

  const savedTranslateAgent = settings?.document_translate_agent ?? null

  const choices = useMemo(() => {
    const enabledAvailable = agents.filter((a) => a.enabled && a.available)
    const savedUnavailable =
      savedTranslateAgent != null &&
      !enabledAvailable.some((a) => a.agent_type === savedTranslateAgent)
        ? (agents.find((a) => a.agent_type === savedTranslateAgent) ?? {
            agent_type: savedTranslateAgent,
            name: savedTranslateAgent,
            enabled: false,
            available: false,
          })
        : null
    return { enabledAvailable, savedUnavailable }
  }, [agents, savedTranslateAgent])

  const selectValue = savedTranslateAgent ?? OFF_VALUE

  const buildApiKeyUpdate = useCallback((): ApiKeyUpdate | undefined => {
    if (keyCleared) return { clear: true }
    const trimmed = keyDraft.trim()
    if (trimmed.length > 0) return { set: trimmed }
    // Blank password alone → Keep (omit field).
    return undefined
  }, [keyCleared, keyDraft])

  const onSaveTitle = useCallback(async () => {
    setSavingTitle(true)
    try {
      const apiKeyUpdate = buildApiKeyUpdate()
      const saved = await setAutoTitleApiConfig({
        api_url: urlDraft,
        ...(apiKeyUpdate != null ? { api_key_update: apiKeyUpdate } : {}),
        model: modelDraft,
      })
      setUrlDraft(saved.auto_title_api_url)
      setModelDraft(saved.auto_title_model)
      setKeyDraft("")
      setKeyCleared(false)
      setTitleRevision(saved.revision)
      setTitleBaseline({
        url: saved.auto_title_api_url,
        model: saved.auto_title_model,
      })
    } catch (err: unknown) {
      toast.error(t("autoTitleSaveFailed", { message: toErrorMessage(err) }))
    } finally {
      setSavingTitle(false)
    }
  }, [buildApiKeyUpdate, modelDraft, setAutoTitleApiConfig, t, urlDraft])

  const onClearKey = useCallback(() => {
    setKeyCleared(true)
    setKeyDraft("")
  }, [])

  const onChangeTranslate = useCallback(
    async (value: string) => {
      const next: AgentType | null =
        value === OFF_VALUE ? null : (value as AgentType)
      setSavingTranslate(true)
      try {
        await setDocumentTranslateAgent(next)
      } catch (err: unknown) {
        toast.error(
          t("documentTranslateSaveFailed", { message: toErrorMessage(err) })
        )
      } finally {
        setSavingTranslate(false)
      }
    },
    [setDocumentTranslateAgent, t]
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

  const showClearKey =
    (settings?.auto_title_api_key_set ?? false) && !keyCleared

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
        <div className="space-y-6">
          {/* Automatic titles — HTTP API */}
          <div className="space-y-3">
            <h3 className="text-sm font-medium">{t("autoTitleSection")}</h3>

            <div className="space-y-2">
              <label
                htmlFor="auto-title-api-url"
                className="text-sm font-medium"
              >
                {t("autoTitleApiUrl")}
              </label>
              <Input
                id="auto-title-api-url"
                type="url"
                autoComplete="off"
                spellCheck={false}
                value={urlDraft}
                disabled={savingTitle || loading}
                onChange={(e) => setUrlDraft(e.target.value)}
                placeholder="https://api.example.com/v1"
              />
            </div>

            <div className="space-y-2">
              <label
                htmlFor="auto-title-api-key"
                className="text-sm font-medium"
              >
                {t("autoTitleApiKey")}
              </label>
              <div className="flex items-center gap-2">
                <Input
                  id="auto-title-api-key"
                  type="password"
                  autoComplete="new-password"
                  value={keyDraft}
                  disabled={savingTitle || loading || keyCleared}
                  onChange={(e) => {
                    setKeyDraft(e.target.value)
                    if (keyCleared) setKeyCleared(false)
                  }}
                  placeholder={
                    keyCleared
                      ? t("autoTitleApiKeyClearedPlaceholder")
                      : settings?.auto_title_api_key_set
                        ? t("autoTitleApiKeySetPlaceholder")
                        : t("autoTitleApiKeyPlaceholder")
                  }
                  className="flex-1"
                />
                {showClearKey && (
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    disabled={savingTitle || loading}
                    onClick={onClearKey}
                    data-testid="auto-title-clear-key"
                  >
                    {t("autoTitleClearKey")}
                  </Button>
                )}
              </div>
            </div>

            <div className="space-y-2">
              <label htmlFor="auto-title-model" className="text-sm font-medium">
                {t("autoTitleModel")}
              </label>
              <Input
                id="auto-title-model"
                type="text"
                autoComplete="off"
                spellCheck={false}
                value={modelDraft}
                disabled={savingTitle || loading}
                onChange={(e) => setModelDraft(e.target.value)}
                placeholder="gpt-4o-mini"
              />
            </div>

            <p
              className="text-xs leading-5"
              data-testid="auto-title-status"
              data-status={
                barrier ? "barrier" : complete ? "enabled" : "incomplete"
              }
            >
              <span
                className={
                  barrier
                    ? "text-amber-600 dark:text-amber-400"
                    : complete
                      ? "text-emerald-600 dark:text-emerald-400"
                      : "text-muted-foreground"
                }
              >
                {statusLabel}
              </span>
            </p>

            <p
              className="text-xs text-muted-foreground leading-5"
              data-testid="title-http-disclosure"
            >
              {t("autoTitleHttpDisclosure")}
            </p>

            <div className="flex justify-end">
              <Button
                type="button"
                size="sm"
                disabled={savingTitle || loading}
                onClick={() => {
                  void onSaveTitle()
                }}
                data-testid="auto-title-save"
              >
                {savingTitle ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  t("autoTitleSave")
                )}
              </Button>
            </div>
          </div>

          {/* Document translation — ACP agent */}
          <div className="space-y-2">
            <div className="flex items-center justify-between gap-3">
              <div className="space-y-1 min-w-0">
                <label
                  htmlFor="document-translate-agent"
                  className="text-sm font-medium"
                >
                  {t("documentTranslateAgent")}
                </label>
              </div>
              <Select
                value={selectValue}
                onValueChange={onChangeTranslate}
                disabled={savingTranslate || loading}
              >
                <SelectTrigger
                  id="document-translate-agent"
                  className="w-[220px] shrink-0"
                  data-testid="document-translate-agent"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={OFF_VALUE}>
                    {t("documentTranslateOff")}
                  </SelectItem>
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
                      {t("documentTranslateUnavailable", {
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

          {/* Reference search limit */}
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
