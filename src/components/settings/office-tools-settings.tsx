"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import {
  BarChart3,
  Box,
  Clapperboard,
  ClipboardList,
  Download,
  FileSpreadsheet,
  FileText,
  GraduationCap,
  Loader2,
  Presentation,
  RefreshCw,
  Rocket,
  Trash2,
  TrendingUp,
  type LucideIcon,
  FileStack,
} from "lucide-react"
import { useLocale, useTranslations } from "next-intl"
import ReactMarkdown from "react-markdown"
import remarkGfm from "remark-gfm"
import { toast } from "sonner"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable"
import { Switch } from "@/components/ui/switch"
import { cn } from "@/lib/utils"
import {
  acpListAgents,
  officecliDetect,
  officecliInstall,
  officecliUninstall,
  officecliListSkills,
  officecliSyncSkills,
  officecliSkillLinkToAgent,
  officecliSkillUnlinkFromAgent,
  officecliSkillGetInstallStatus,
  officecliSkillReadContent,
} from "@/lib/api"
import { invalidateAgentExpertsCache } from "@/hooks/use-agent-experts"
import type {
  AcpAgentInfo,
  AgentType,
  ExpertInstallStatus,
  ExpertLinkState,
  OfficecliInfo,
  OfficecliSkill,
} from "@/lib/types"
import { toErrorMessage } from "@/lib/app-error"

const ICON_MAP: Record<string, LucideIcon> = {
  FileStack,
  Presentation,
  Rocket,
  Clapperboard,
  Box,
  FileText,
  GraduationCap,
  ClipboardList,
  FileSpreadsheet,
  TrendingUp,
  BarChart3,
}

const CATEGORY_SORT: Record<string, number> = {
  general: 0,
  presentations: 1,
  documents: 2,
  spreadsheets: 3,
}

const LEFT_MIN_WIDTH = 320
const RIGHT_MIN_WIDTH = 440

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value))
}

function toPercent(pixels: number, totalPixels: number): number {
  if (totalPixels <= 0) return 0
  return (pixels / totalPixels) * 100
}

function pickLocalized(
  dict: Record<string, string> | undefined,
  locale: string
): string {
  if (!dict) return ""
  if (dict[locale]) return dict[locale]
  const normalized = locale.replace("_", "-")
  if (dict[normalized]) return dict[normalized]
  const [lang] = normalized.split("-")
  const match = Object.keys(dict).find(
    (key) => key.toLowerCase().split("-")[0] === lang.toLowerCase()
  )
  if (match) return dict[match]
  return dict.en ?? Object.values(dict)[0] ?? ""
}

function stripFrontmatter(content: string): string {
  const match = content.match(/^---\s*\r?\n[\s\S]*?\r?\n---\s*(?:\r?\n)?/)
  if (!match) return content
  return content.slice(match[0].length)
}

function getIcon(name: string | null | undefined): LucideIcon {
  if (name && ICON_MAP[name]) return ICON_MAP[name]
  return FileStack
}

// ─── Detection card ───────────────────────────────────────────────────

function DetectionCard({
  info,
  detecting,
  installing,
  onInstall,
  onUninstall,
  onSync,
  syncing,
}: {
  info: OfficecliInfo | null
  detecting: boolean
  installing: boolean
  onInstall: () => void
  onUninstall: () => void
  onSync: () => void
  syncing: boolean
}) {
  const t = useTranslations("OfficeToolsSettings")
  const installed = info?.installed === true

  return (
    <div
      className={cn(
        "rounded-lg border p-4",
        installed
          ? "border-green-500/30 bg-green-500/5"
          : "border-muted bg-muted/5"
      )}
    >
      <div className="flex items-center justify-between gap-4">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="text-sm font-semibold">OfficeCLI</h3>
            {detecting ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
            ) : installed ? (
              <Badge
                variant="outline"
                className="h-5 px-1.5 text-[10px] border-green-500/40 bg-green-500/10 text-green-600 dark:text-green-400"
              >
                {t("detection.installed")}
              </Badge>
            ) : (
              <Badge
                variant="outline"
                className="h-5 px-1.5 text-[10px] text-muted-foreground"
              >
                {t("detection.notInstalled")}
              </Badge>
            )}
          </div>
          {installed && info && (
            <div className="text-[11px] text-muted-foreground mt-1 space-x-3">
              {info.version && <span>{info.version}</span>}
              {info.path && (
                <code className="font-mono text-[10px]">{info.path}</code>
              )}
            </div>
          )}
          {!installed && !detecting && (
            <p className="text-xs text-muted-foreground mt-1">
              {t("detection.installHint")}
            </p>
          )}
        </div>
        <div className="flex items-center gap-2 shrink-0">
          {installed ? (
            <>
              <Button
                size="sm"
                variant="outline"
                disabled={syncing}
                onClick={onSync}
              >
                {syncing ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <RefreshCw className="h-3.5 w-3.5" />
                )}
                {t("detection.syncSkills")}
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="text-destructive hover:text-destructive"
                onClick={onUninstall}
              >
                <Trash2 className="h-3.5 w-3.5" />
                {t("detection.uninstall")}
              </Button>
            </>
          ) : (
            <Button
              size="sm"
              disabled={installing || detecting}
              onClick={onInstall}
            >
              {installing ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Download className="h-3.5 w-3.5" />
              )}
              {t("detection.install")}
            </Button>
          )}
        </div>
      </div>
    </div>
  )
}

// ─── Main component ───────────────────────────────────────────────────

export function OfficeToolsSettings() {
  const t = useTranslations("OfficeToolsSettings")
  const locale = useLocale()
  const panelContainerRef = useRef<HTMLDivElement | null>(null)
  const [panelContainerWidth, setPanelContainerWidth] = useState(0)

  const [info, setInfo] = useState<OfficecliInfo | null>(null)
  const [detecting, setDetecting] = useState(true)
  const [installing, setInstalling] = useState(false)
  const [syncing, setSyncing] = useState(false)

  const [skills, setSkills] = useState<OfficecliSkill[]>([])
  const [agents, setAgents] = useState<AcpAgentInfo[]>([])
  const [selectedSkillId, setSelectedSkillId] = useState<string | null>(null)
  const [searchQuery, setSearchQuery] = useState("")

  const [content, setContent] = useState<string>("")
  const [contentLoading, setContentLoading] = useState(false)

  const [statuses, setStatuses] = useState<Record<string, ExpertInstallStatus>>(
    {}
  )
  const [statusLoading, setStatusLoading] = useState(false)
  const [pendingMutation, setPendingMutation] = useState<string | null>(null)

  const translatedState = useCallback(
    (state: ExpertLinkState): string => {
      switch (state) {
        case "not_linked":
          return t("states.not_linked")
        case "linked_to_codeg":
          return t("states.linked_to_codeg")
        case "linked_elsewhere":
          return t("states.linked_elsewhere")
        case "blocked_by_real_directory":
          return t("states.blocked_by_real_directory")
        case "broken":
          return t("states.broken")
        default:
          return state
      }
    },
    [t]
  )

  const translatedCategory = useCallback(
    (category: string): string => {
      switch (category) {
        case "general":
          return t("categories.general")
        case "presentations":
          return t("categories.presentations")
        case "documents":
          return t("categories.documents")
        case "spreadsheets":
          return t("categories.spreadsheets")
        default:
          return category
      }
    },
    [t]
  )

  const detect = useCallback(async () => {
    setDetecting(true)
    try {
      const result = await officecliDetect()
      setInfo(result)
    } catch {
      setInfo(null)
    } finally {
      setDetecting(false)
    }
  }, [])

  const refreshSkills = useCallback(async () => {
    try {
      const [skillList, agentList] = await Promise.all([
        officecliListSkills(),
        acpListAgents(),
      ])
      setSkills(skillList)
      setAgents(agentList)
    } catch (err) {
      const message = toErrorMessage(err)
      toast.error(t("toasts.loadFailed"), { description: message })
    }
  }, [t])

  useEffect(() => {
    Promise.all([detect(), refreshSkills()]).catch((err) => {
      console.error("[OfficeToolsSettings] initial load failed:", err)
    })
  }, [detect, refreshSkills])

  useEffect(() => {
    const container = panelContainerRef.current
    if (!container) return
    const updateWidth = (next: number) => {
      setPanelContainerWidth((prev) =>
        Math.abs(prev - next) < 1 ? prev : next
      )
    }
    updateWidth(container.getBoundingClientRect().width)
    const observer = new ResizeObserver((entries) => {
      updateWidth(
        entries[0]?.contentRect.width ?? container.getBoundingClientRect().width
      )
    })
    observer.observe(container)
    return () => {
      observer.disconnect()
    }
  }, [])

  const sortedSkills = useMemo(() => {
    return [...skills].sort((a, b) => {
      const ca = CATEGORY_SORT[a.category] ?? 99
      const cb = CATEGORY_SORT[b.category] ?? 99
      if (ca !== cb) return ca - cb
      return a.sortOrder - b.sortOrder
    })
  }, [skills])

  const filteredSkills = useMemo(() => {
    const q = searchQuery.trim().toLowerCase()
    if (!q) return sortedSkills
    return sortedSkills.filter((item) => {
      const name = pickLocalized(item.displayName, locale)
      const desc = pickLocalized(item.description, locale)
      return (
        item.id.toLowerCase().includes(q) ||
        name.toLowerCase().includes(q) ||
        desc.toLowerCase().includes(q)
      )
    })
  }, [sortedSkills, searchQuery, locale])

  const groupedSkills = useMemo(() => {
    const groups = new Map<string, OfficecliSkill[]>()
    for (const item of filteredSkills) {
      const key = item.category
      const list = groups.get(key) ?? []
      list.push(item)
      groups.set(key, list)
    }
    return Array.from(groups.entries()).sort(
      (a, b) => (CATEGORY_SORT[a[0]] ?? 99) - (CATEGORY_SORT[b[0]] ?? 99)
    )
  }, [filteredSkills])

  const selectedSkill = useMemo(
    () => skills.find((s) => s.id === selectedSkillId) ?? null,
    [skills, selectedSkillId]
  )

  useEffect(() => {
    if (!selectedSkillId && sortedSkills.length > 0) {
      setSelectedSkillId(sortedSkills[0].id)
    }
  }, [selectedSkillId, sortedSkills])

  useEffect(() => {
    if (!selectedSkill) {
      setContent("")
      setStatuses({})
      return
    }
    const skillId = selectedSkill.id
    let cancelled = false
    setContentLoading(true)
    setStatusLoading(true)

    const loadContent = selectedSkill.installedCentrally
      ? officecliSkillReadContent(skillId).catch(() => "")
      : Promise.resolve("")

    Promise.all([loadContent, officecliSkillGetInstallStatus(skillId)])
      .then(([body, statusList]) => {
        if (cancelled) return
        setContent(body)
        const map: Record<string, ExpertInstallStatus> = {}
        for (const entry of statusList) {
          map[entry.agentType] = entry
        }
        setStatuses(map)
      })
      .catch((err) => {
        if (cancelled) return
        const message = toErrorMessage(err)
        toast.error(t("toasts.loadFailed"), { description: message })
      })
      .finally(() => {
        if (!cancelled) {
          setContentLoading(false)
          setStatusLoading(false)
        }
      })
    return () => {
      cancelled = true
    }
  }, [selectedSkill, t])

  const handleToggle = useCallback(
    async (skillId: string, agentType: AgentType, enable: boolean) => {
      const key = `${skillId}:${agentType}`
      setPendingMutation(key)
      try {
        if (enable) {
          const next = await officecliSkillLinkToAgent({ skillId, agentType })
          setStatuses((prev) => ({ ...prev, [agentType]: next }))
          invalidateAgentExpertsCache(agentType)
          toast.success(t("toasts.enabled"))
        } else {
          await officecliSkillUnlinkFromAgent({ skillId, agentType })
          const latest = await officecliSkillGetInstallStatus(skillId)
          const map: Record<string, ExpertInstallStatus> = {}
          for (const entry of latest) {
            map[entry.agentType] = entry
          }
          setStatuses(map)
          invalidateAgentExpertsCache(agentType)
          toast.success(t("toasts.disabled"))
        }
      } catch (err) {
        const message = toErrorMessage(err)
        toast.error(
          enable ? t("toasts.enableFailed") : t("toasts.disableFailed"),
          { description: message }
        )
      } finally {
        setPendingMutation(null)
      }
    },
    [t]
  )

  const handleInstall = useCallback(async () => {
    setInstalling(true)
    try {
      const result = await officecliInstall()
      setInfo(result)
      toast.success(t("toasts.installSuccess"))
      await officecliSyncSkills()
      await refreshSkills()
    } catch (err) {
      const message = toErrorMessage(err)
      toast.error(t("toasts.installFailed"), { description: message })
    } finally {
      setInstalling(false)
    }
  }, [t, refreshSkills])

  const handleUninstall = useCallback(async () => {
    try {
      const result = await officecliUninstall()
      setInfo(result)
      await refreshSkills()
      toast.success(t("toasts.uninstallSuccess"))
    } catch (err) {
      const message = toErrorMessage(err)
      toast.error(t("toasts.uninstallFailed"), { description: message })
    }
  }, [t, refreshSkills])

  const handleSync = useCallback(async () => {
    setSyncing(true)
    try {
      const report = await officecliSyncSkills()
      await refreshSkills()
      if (report.errors.length > 0) {
        toast.warning(
          t("toasts.syncPartial", {
            synced: report.synced,
            errors: report.errors.length,
          })
        )
      } else {
        toast.success(t("toasts.syncSuccess", { synced: report.synced }))
      }
    } catch (err) {
      const message = toErrorMessage(err)
      toast.error(t("toasts.syncFailed"), { description: message })
    } finally {
      setSyncing(false)
    }
  }, [t, refreshSkills])

  const safeContainerWidth =
    panelContainerWidth > 0 ? panelContainerWidth : 1200
  const leftMinSize = clamp(
    toPercent(LEFT_MIN_WIDTH, safeContainerWidth),
    5,
    95
  )
  const rightMinSize = clamp(
    toPercent(RIGHT_MIN_WIDTH, safeContainerWidth),
    5,
    95
  )
  const leftMaxSize = Math.max(leftMinSize, 100 - rightMinSize)

  const installed = info?.installed === true
  const selectedName = selectedSkill
    ? pickLocalized(selectedSkill.displayName, locale) || selectedSkill.id
    : ""
  const selectedDescription = selectedSkill
    ? pickLocalized(selectedSkill.description, locale)
    : ""
  const SelectedIcon = getIcon(selectedSkill?.icon ?? null)

  return (
    <div className="h-full flex flex-col p-3 md:p-4">
      <div className="flex items-center justify-between gap-3 pb-4">
        <div>
          <h2 className="text-base font-semibold">{t("title")}</h2>
          <p className="text-xs text-muted-foreground mt-1">
            {t("description")}
          </p>
        </div>
        <Button
          size="sm"
          variant="outline"
          onClick={() => {
            Promise.all([detect(), refreshSkills()]).catch((err) => {
              console.error("[OfficeToolsSettings] refresh failed:", err)
            })
          }}
        >
          <RefreshCw className="h-3.5 w-3.5" />
          {t("actions.refresh")}
        </Button>
      </div>

      <DetectionCard
        info={info}
        detecting={detecting}
        installing={installing}
        onInstall={handleInstall}
        onUninstall={handleUninstall}
        onSync={handleSync}
        syncing={syncing}
      />

      <div ref={panelContainerRef} className="flex-1 min-h-0 min-w-0 mt-4">
        {skills.length === 0 ? (
          <div className="h-full rounded-lg border bg-card flex items-center justify-center text-sm text-muted-foreground">
            {t("emptySkills")}
          </div>
        ) : (
          <ResizablePanelGroup
            direction="horizontal"
            className="h-full min-h-0 min-w-0"
          >
            <ResizablePanel
              defaultSize={38}
              minSize={leftMinSize}
              maxSize={leftMaxSize}
            >
              <div className="min-h-0 h-full min-w-0 rounded-lg border bg-card flex flex-col overflow-hidden lg:rounded-r-none">
                <div className="border-b p-3 space-y-2.5">
                  <Input
                    value={searchQuery}
                    onChange={(event) => setSearchQuery(event.target.value)}
                    placeholder={t("searchPlaceholder")}
                  />
                </div>
                <div className="flex-1 min-h-0 overflow-y-auto p-2 space-y-3">
                  {groupedSkills.map(([category, items]) => (
                    <div key={category} className="space-y-1.5">
                      <div className="px-1 text-[11px] uppercase tracking-wide font-semibold text-muted-foreground">
                        {translatedCategory(category)}
                      </div>
                      {items.map((item) => {
                        const Icon = getIcon(item.icon)
                        const name =
                          pickLocalized(item.displayName, locale) || item.id
                        const desc = pickLocalized(item.description, locale)
                        const isActive = selectedSkillId === item.id
                        return (
                          <button
                            key={item.id}
                            type="button"
                            onClick={() => setSelectedSkillId(item.id)}
                            className={cn(
                              "w-full rounded-md border px-2.5 py-2 text-left transition-colors",
                              isActive
                                ? "border-primary/60 bg-primary/5"
                                : "hover:bg-muted/30"
                            )}
                          >
                            <div className="flex items-start gap-2 min-w-0">
                              <Icon className="h-4 w-4 mt-0.5 shrink-0 text-primary/80" />
                              <div className="min-w-0 flex-1">
                                <div className="text-sm font-medium truncate">
                                  {name}
                                </div>
                                <div className="text-[11px] text-muted-foreground line-clamp-2 mt-0.5">
                                  {desc}
                                </div>
                              </div>
                              {!item.installedCentrally && (
                                <Badge
                                  variant="outline"
                                  className="h-5 px-1.5 text-[10px] shrink-0 text-muted-foreground"
                                >
                                  {t("badges.notSynced")}
                                </Badge>
                              )}
                            </div>
                          </button>
                        )
                      })}
                    </div>
                  ))}
                  {groupedSkills.length === 0 && (
                    <div className="text-xs text-muted-foreground px-2 py-3">
                      {t("emptySearch")}
                    </div>
                  )}
                </div>
              </div>
            </ResizablePanel>

            <ResizableHandle withHandle />

            <ResizablePanel defaultSize={62} minSize={rightMinSize}>
              <div className="h-full flex-1 min-h-0 min-w-0 rounded-lg border bg-card overflow-hidden lg:rounded-l-none lg:border-l-0">
                {selectedSkill ? (
                  <div className="h-full flex flex-col">
                    <div className="border-b px-4 py-3 flex items-start gap-3">
                      <SelectedIcon className="h-5 w-5 mt-0.5 shrink-0 text-primary/80" />
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2">
                          <h3 className="text-sm font-semibold truncate">
                            {selectedName}
                          </h3>
                          <Badge
                            variant="outline"
                            className="h-5 px-1.5 text-[10px]"
                          >
                            {translatedCategory(selectedSkill.category)}
                          </Badge>
                          <code className="text-[11px] text-muted-foreground font-mono truncate">
                            {selectedSkill.id}
                          </code>
                        </div>
                        <p className="text-xs text-muted-foreground mt-1">
                          {selectedDescription}
                        </p>
                      </div>
                    </div>

                    <div className="flex-1 overflow-y-auto p-4 space-y-4">
                      <div className="rounded-md border p-3">
                        <div className="text-[11px] text-muted-foreground mb-2 flex items-center justify-between">
                          <span>{t("enableForAgents")}</span>
                          {statusLoading && (
                            <Loader2 className="h-3 w-3 animate-spin" />
                          )}
                        </div>
                        {!installed ? (
                          <div className="text-xs text-muted-foreground py-2">
                            {t("installFirst")}
                          </div>
                        ) : !selectedSkill.installedCentrally ? (
                          <div className="text-xs text-muted-foreground py-2">
                            {t("syncFirst")}
                          </div>
                        ) : agents.length === 0 ? (
                          <div className="text-xs text-muted-foreground py-2">
                            {t("noAgents")}
                          </div>
                        ) : (
                          <div className="space-y-1.5">
                            {agents.map((agent) => {
                              const status = statuses[agent.agent_type] ?? null
                              const enabled =
                                status?.state === "linked_to_codeg"
                              const blocked =
                                status?.state === "blocked_by_real_directory" ||
                                status?.state === "linked_elsewhere"
                              const key = `${selectedSkill.id}:${agent.agent_type}`
                              const pending = pendingMutation === key
                              return (
                                <div
                                  key={agent.agent_type}
                                  className={cn(
                                    "flex items-center gap-3 rounded-md border px-3 py-2",
                                    enabled
                                      ? "border-primary/40 bg-primary/5"
                                      : "border-border"
                                  )}
                                >
                                  <div className="flex-1 min-w-0">
                                    <div className="text-sm font-medium truncate">
                                      {agent.name}
                                    </div>
                                    <div className="text-[11px] text-muted-foreground truncate">
                                      {status
                                        ? translatedState(status.state)
                                        : "—"}
                                    </div>
                                    {status?.copyMode && (
                                      <div className="text-[11px] text-amber-500 mt-0.5">
                                        {t("copyModeWarning")}
                                      </div>
                                    )}
                                  </div>
                                  <Switch
                                    checked={enabled}
                                    disabled={pending || (blocked && !enabled)}
                                    onCheckedChange={(checked: boolean) => {
                                      handleToggle(
                                        selectedSkill.id,
                                        agent.agent_type,
                                        checked
                                      ).catch((err) => {
                                        console.error(
                                          "[OfficeToolsSettings] toggle failed:",
                                          err
                                        )
                                      })
                                    }}
                                  />
                                </div>
                              )
                            })}
                          </div>
                        )}
                      </div>

                      {selectedSkill.installedCentrally && (
                        <div className="rounded-md border p-3">
                          <div className="text-[11px] text-muted-foreground mb-2">
                            {t("previewTitle")}
                          </div>
                          {contentLoading ? (
                            <div className="flex items-center gap-2 text-xs text-muted-foreground py-3">
                              <Loader2 className="h-3.5 w-3.5 animate-spin" />
                              {t("loadingContent")}
                            </div>
                          ) : content ? (
                            <div
                              className={cn(
                                "text-sm leading-6 rounded-md bg-muted/10 p-3 overflow-auto max-h-[480px]",
                                "[&_h1]:text-xl [&_h1]:font-semibold [&_h1]:mb-3",
                                "[&_h2]:text-lg [&_h2]:font-semibold [&_h2]:mt-5 [&_h2]:mb-2",
                                "[&_h3]:text-base [&_h3]:font-semibold [&_h3]:mt-4 [&_h3]:mb-2",
                                "[&_p]:mb-3 [&_li]:mb-1",
                                "[&_ul]:list-disc [&_ul]:pl-5 [&_ol]:list-decimal [&_ol]:pl-5",
                                "[&_code]:font-mono [&_code]:text-xs [&_code]:bg-muted [&_code]:rounded [&_code]:px-1",
                                "[&_pre]:bg-muted [&_pre]:rounded-md [&_pre]:p-3 [&_pre]:overflow-x-auto"
                              )}
                            >
                              <ReactMarkdown remarkPlugins={[remarkGfm]}>
                                {stripFrontmatter(content)}
                              </ReactMarkdown>
                            </div>
                          ) : (
                            <div className="text-xs text-muted-foreground py-2">
                              {t("noContent")}
                            </div>
                          )}
                        </div>
                      )}
                    </div>
                  </div>
                ) : (
                  <div className="h-full flex items-center justify-center text-xs text-muted-foreground">
                    {t("emptySelection")}
                  </div>
                )}
              </div>
            </ResizablePanel>
          </ResizablePanelGroup>
        )}
      </div>
    </div>
  )
}
