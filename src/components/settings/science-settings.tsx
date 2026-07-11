"use client"

import { useCallback, useEffect, useMemo, useState } from "react"
import { FolderOpen, Loader2, RefreshCw } from "lucide-react"
import { useLocale, useTranslations } from "next-intl"
import { toast } from "sonner"

import { Button } from "@/components/ui/button"
import {
  SkillAgentMatrix,
  type MatrixSkill,
} from "@/components/settings/skill-agent-matrix"
import {
  acpListAgents,
  openFolder,
  scienceApplyLinks,
  scienceList,
  scienceListAllInstallStatuses,
  scienceOpenCentralDir,
  scienceReadContent,
} from "@/lib/api"
import { revealItemInDir } from "@/lib/platform"
import { getActiveRemoteConnectionId, isDesktop } from "@/lib/transport"
import { invalidateAgentSkillsCache } from "@/hooks/use-agent-skills"
import { piUsesCustomAgentDir } from "@/lib/pi-config"
import type {
  AcpAgentInfo,
  ExpertLinkState,
  ScienceListItem,
} from "@/lib/types"
import { toErrorMessage } from "@/lib/app-error"
import { pickLocalized } from "@/lib/expert-presentation"
import { getScienceIcon } from "@/lib/science-presentation"

const CATEGORY_SORT: Record<string, number> = {
  ideation: 1,
  design: 2,
  analysis: 3,
  visualization: 4,
  evaluation: 5,
  literature: 6,
}

export function ScienceSettings() {
  const t = useTranslations("ScienceSettings")
  const locale = useLocale()

  const [skills, setSkills] = useState<ScienceListItem[]>([])
  const [agents, setAgents] = useState<AcpAgentInfo[]>([])
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [reloadKey, setReloadKey] = useState(0)

  const refresh = useCallback(async () => {
    setLoading(true)
    setLoadError(null)
    try {
      const [skillList, agentList] = await Promise.all([
        scienceList(),
        acpListAgents(),
      ])
      setSkills(skillList)
      // A pi pointed at a custom PI_CODING_AGENT_DIR isn't managed by the
      // default-dir skill store, so it doesn't get a column here.
      setAgents(agentList.filter((agent) => !piUsesCustomAgentDir(agent)))
      setReloadKey((k) => k + 1)
    } catch (err) {
      setLoadError(toErrorMessage(err))
      setSkills([])
      setAgents([])
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    refresh().catch((err) => {
      console.error("[ScienceSettings] initial refresh failed:", err)
    })
  }, [refresh])

  const translatedCategory = useCallback(
    (category: string): string => {
      switch (category) {
        case "ideation":
          return t("categories.ideation")
        case "design":
          return t("categories.design")
        case "analysis":
          return t("categories.analysis")
        case "visualization":
          return t("categories.visualization")
        case "evaluation":
          return t("categories.evaluation")
        case "literature":
          return t("categories.literature")
        default:
          return category
      }
    },
    [t]
  )

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

  const matrixSkills = useMemo<MatrixSkill[]>(
    () =>
      skills.map((s) => {
        // Single-badge priority: a user edit (pending review) wins, then the
        // "needs an API key" hint, then the softer "may need a Python setup".
        const badge: MatrixSkill["badge"] = s.user_modified
          ? { label: t("badges.userModified"), tone: "amber" }
          : s.metadata.needs_key
            ? { label: t("badges.needsKey"), tone: "amber" }
            : s.metadata.needs_env
              ? { label: t("badges.needsSetup"), tone: "muted" }
              : undefined
        return {
          id: s.metadata.id,
          category: s.metadata.category,
          displayName:
            pickLocalized(s.metadata.display_name, locale) || s.metadata.id,
          description: pickLocalized(s.metadata.description, locale),
          icon: getScienceIcon(s.metadata.icon),
          ready: true,
          badge,
        }
      }),
    [skills, locale, t]
  )

  const handleOpenCentralDir = useCallback(async () => {
    try {
      const path = await scienceOpenCentralDir()
      if (isDesktop() && getActiveRemoteConnectionId() === null) {
        // Desktop: reveal the central skills folder. `revealItemInDir` (not
        // `openPath`) is used deliberately — the opener plugin's path scope
        // rejects `openPath` for the hidden `~/.codeg/...` path.
        await revealItemInDir(path)
      } else {
        await openFolder(path)
      }
    } catch (err) {
      toast.error(t("toasts.openFolderFailed"), {
        description: toErrorMessage(err),
      })
    }
  }, [t])

  if (loading) {
    return (
      <div className="h-full flex items-center justify-center text-sm text-muted-foreground">
        <Loader2 className="h-4 w-4 mr-2 animate-spin" />
        {t("loading")}
      </div>
    )
  }

  return (
    <div className="h-full flex flex-col p-3 md:p-4">
      <div className="flex items-center justify-between gap-3 pb-4">
        <div>
          <h2 className="text-base font-semibold">{t("title")}</h2>
          <p className="text-xs text-muted-foreground mt-1">
            {t("description")}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            size="sm"
            variant="outline"
            onClick={() => {
              handleOpenCentralDir().catch((err) => {
                console.error("[ScienceSettings] open central dir failed:", err)
              })
            }}
          >
            <FolderOpen className="h-3.5 w-3.5" />
            {t("actions.openCentralDir")}
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => {
              refresh().catch((err) => {
                console.error("[ScienceSettings] refresh failed:", err)
              })
            }}
          >
            <RefreshCw className="h-3.5 w-3.5" />
            {t("actions.refresh")}
          </Button>
        </div>
      </div>

      {loadError && (
        <div className="mb-3 rounded-md border border-red-500/30 bg-red-500/5 px-3 py-2 text-xs text-red-400">
          {loadError}
        </div>
      )}

      {skills.length === 0 ? (
        <div className="h-full rounded-lg border bg-card flex items-center justify-center text-sm text-muted-foreground">
          {t("emptySkills")}
        </div>
      ) : (
        <div className="flex-1 min-h-0 min-w-0">
          <SkillAgentMatrix
            key={reloadKey}
            skills={matrixSkills}
            agents={agents}
            categoryOrder={CATEGORY_SORT}
            translateCategory={translatedCategory}
            translateState={translatedState}
            loadAllStatuses={scienceListAllInstallStatuses}
            applyLinks={scienceApplyLinks}
            loadContent={scienceReadContent}
            onApplied={(touched) =>
              touched.forEach((a) => invalidateAgentSkillsCache(a))
            }
            searchPlaceholder={t("searchPlaceholder")}
          />
        </div>
      )}
    </div>
  )
}
