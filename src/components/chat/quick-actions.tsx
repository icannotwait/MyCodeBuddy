"use client"

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react"
import { useLocale, useTranslations } from "next-intl"
import {
  BarChart3,
  Box,
  Clapperboard,
  FileSpreadsheet,
  FileText,
  GraduationCap,
  Presentation,
  Rocket,
  TrendingUp,
  type LucideIcon,
} from "lucide-react"

import { cn } from "@/lib/utils"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { useBuiltInExperts } from "@/hooks/use-built-in-experts"
import { getExpertIcon, pickLocalized } from "@/lib/expert-presentation"
import {
  loadQuickActionsTab,
  saveQuickActionsTab,
  type QuickActionsTab,
} from "@/lib/quick-actions-tab-storage"
import type { ComposerInjectContent } from "@/components/chat/message-input"
import type { ExpertListItem } from "@/lib/types"

interface OfficeAction {
  /** Stable id; also the i18n label key (`<id>`) and description (`<id>Desc`). */
  id: string
  icon: LucideIcon
  /** i18n key under `prompts.*` for the localized prompt template. */
  promptKey: string
  /** OfficeCLI skill invocation id, prepended as a leading badge on click. */
  skillId: string
}

// Three primary office categories — prominent fixed cards (keep their color).
const OFFICE_FIXED: (OfficeAction & { accent: string })[] = [
  {
    id: "excel",
    icon: FileSpreadsheet,
    promptKey: "prompts.excel",
    skillId: "officecli-xlsx",
    accent: "green",
  },
  {
    id: "word",
    icon: FileText,
    promptKey: "prompts.word",
    skillId: "officecli-docx",
    accent: "blue",
  },
  {
    id: "ppt",
    icon: Presentation,
    promptKey: "prompts.ppt",
    skillId: "officecli-pptx",
    accent: "orange",
  },
]

// Remaining office skills — de-colored bars in the scrolling row (icons kept).
const OFFICE_SCROLL: OfficeAction[] = [
  {
    id: "pitchDeck",
    icon: Rocket,
    promptKey: "prompts.pitchDeck",
    skillId: "officecli-pitch-deck",
  },
  {
    id: "morph",
    icon: Clapperboard,
    promptKey: "prompts.morph",
    skillId: "morph-ppt",
  },
  {
    id: "morph3d",
    icon: Box,
    promptKey: "prompts.morph3d",
    skillId: "morph-ppt-3d",
  },
  {
    id: "academic",
    icon: GraduationCap,
    promptKey: "prompts.academic",
    skillId: "officecli-academic-paper",
  },
  {
    id: "financial",
    icon: TrendingUp,
    promptKey: "prompts.financial",
    skillId: "officecli-financial-model",
  },
  {
    id: "dashboard",
    icon: BarChart3,
    promptKey: "prompts.dashboard",
    skillId: "officecli-data-dashboard",
  },
]

// Three featured coding experts get prominent fixed cards (color + curated
// short description); the rest fill the scrolling row. ids match the bundled
// superpowers skills.
const CODING_FEATURED: { id: string; accent: string; descKey: string }[] = [
  { id: "brainstorming", accent: "amber", descKey: "coding.brainstormingDesc" },
  {
    id: "systematic-debugging",
    accent: "pink",
    descKey: "coding.debuggingDesc",
  },
  {
    id: "writing-skills",
    accent: "purple",
    descKey: "coding.writingSkillsDesc",
  },
]
const CODING_FEATURED_IDS = new Set(CODING_FEATURED.map((f) => f.id))

// Static accent class fragments — full literals so Tailwind's JIT keeps them
// (never compose color classes from template strings, those get purged). Only
// the prominent fixed cards are colored; scrolling bars are neutral.
const ACCENTS: Record<string, { icon: string; surface: string }> = {
  green: {
    icon: "text-green-600 dark:text-green-400",
    surface:
      "border-green-500/20 hover:border-green-500/40 hover:bg-green-500/5",
  },
  blue: {
    icon: "text-blue-600 dark:text-blue-400",
    surface: "border-blue-500/20 hover:border-blue-500/40 hover:bg-blue-500/5",
  },
  orange: {
    icon: "text-orange-600 dark:text-orange-400",
    surface:
      "border-orange-500/20 hover:border-orange-500/40 hover:bg-orange-500/5",
  },
  amber: {
    icon: "text-amber-600 dark:text-amber-400",
    surface:
      "border-amber-500/20 hover:border-amber-500/40 hover:bg-amber-500/5",
  },
  pink: {
    icon: "text-pink-600 dark:text-pink-400",
    surface: "border-pink-500/20 hover:border-pink-500/40 hover:bg-pink-500/5",
  },
  purple: {
    icon: "text-purple-600 dark:text-purple-400",
    surface:
      "border-purple-500/20 hover:border-purple-500/40 hover:bg-purple-500/5",
  },
}

/** A prominent fixed category card (icon + title + one-line description). */
function BigCard({
  icon: Icon,
  accent,
  title,
  description,
  onClick,
}: {
  icon: LucideIcon
  accent: string
  title: string
  description: string
  onClick: () => void
}) {
  const a = ACCENTS[accent] ?? ACCENTS.green
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "group flex flex-col items-start gap-1.5 rounded-lg border bg-card/50 px-3 py-2.5 text-left transition-colors",
        a.surface
      )}
    >
      <Icon aria-hidden className={cn("h-4 w-4 transition-colors", a.icon)} />
      <div className="text-sm font-medium text-foreground">{title}</div>
      <div className="line-clamp-1 text-xs text-muted-foreground">
        {description}
      </div>
    </button>
  )
}

/** A neutral (de-colored) skill bar in the scrolling row. */
function SkillBar({
  icon: Icon,
  label,
  title,
  onClick,
  clone,
}: {
  icon: LucideIcon
  label: string
  title?: string
  onClick: () => void
  /** A marquee duplicate: hidden from a11y + keyboard, removed under reduced motion. */
  clone?: boolean
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      aria-hidden={clone || undefined}
      tabIndex={clone ? -1 : undefined}
      data-qa-clone={clone ? "" : undefined}
      className="group mr-2 flex shrink-0 items-center gap-2 rounded-lg border border-border bg-card/50 px-3 py-2 transition-colors hover:border-foreground/20 hover:bg-accent/40"
    >
      <Icon
        aria-hidden
        className="h-4 w-4 shrink-0 text-muted-foreground transition-colors group-hover:text-foreground"
      />
      <span className="whitespace-nowrap text-xs font-medium text-foreground/90">
        {label}
      </span>
    </button>
  )
}

/**
 * Single-row seamless marquee. Renders the items twice (real + an aria-hidden
 * clone) and a requestAnimationFrame loop translates the track left at a
 * constant speed, wrapping by exactly one copy width (`scrollWidth / 2`, exact
 * because each bar carries its own `mr-2` instead of a flex gap) so the loop
 * has no seam.
 *
 * Driving the transform from JS — rather than a CSS animation — is deliberate:
 * a composited CSS transform animation snaps backwards when paused on hover
 * (WebKit reverts to the lagging main-thread clock). Setting the transform
 * ourselves each frame means "pause" just stops updating, freezing it in place.
 * Pauses on pointer hover / keyboard focus; bails entirely under reduced motion
 * (CSS then makes the viewport a normal horizontal scroller).
 */
function Marquee({
  itemCount,
  children,
}: {
  itemCount: number
  children: (clone: boolean) => ReactNode
}) {
  const trackRef = useRef<HTMLDivElement | null>(null)
  const pausedRef = useRef(false)

  useEffect(() => {
    const track = trackRef.current
    if (!track || typeof window === "undefined") return
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return

    const SPEED = 35 // px per second — constant feel regardless of item count
    let offset = 0
    let half = track.scrollWidth / 2
    let last = 0
    let raf = 0

    const ro = new ResizeObserver(() => {
      half = track.scrollWidth / 2
    })
    ro.observe(track)

    const step = (now: number) => {
      raf = requestAnimationFrame(step)
      // Clamp dt so returning from a backgrounded tab doesn't lurch forward.
      const dt = last === 0 ? 0 : Math.min(0.05, (now - last) / 1000)
      last = now
      if (pausedRef.current || half <= 0) return
      offset -= SPEED * dt
      if (offset <= -half) offset += half
      track.style.transform = `translate3d(${offset}px, 0, 0)`
    }
    raf = requestAnimationFrame(step)

    return () => {
      cancelAnimationFrame(raf)
      ro.disconnect()
    }
  }, [itemCount])

  if (itemCount === 0) return null

  const pause = () => {
    pausedRef.current = true
  }
  const resume = () => {
    pausedRef.current = false
  }

  return (
    <div
      className="qa-marquee-viewport overflow-hidden pb-1"
      onPointerEnter={pause}
      onPointerLeave={resume}
      onFocus={pause}
      onBlur={resume}
    >
      <div ref={trackRef} className="qa-marquee-track flex w-max">
        {children(false)}
        {children(true)}
      </div>
    </div>
  )
}

interface QuickActionsProps {
  /** Emits the resolved (localized) injection payload for the picked action. */
  onSelect: (payload: ComposerInjectContent) => void
}

export function QuickActions({ onSelect }: QuickActionsProps) {
  const t = useTranslations("Folder.chat.welcomePanel.quickActions")
  const locale = useLocale()
  const experts = useBuiltInExperts()

  // Restore the last-picked tab; persist on change. The lazy initializer reads
  // localStorage on each mount (QuickActions only renders client-side in
  // welcome mode, so there is no SSR hydration mismatch), so reopening a new
  // conversation shows the previous choice.
  const [tab, setTab] = useState<QuickActionsTab>(() => loadQuickActionsTab())
  const handleTabChange = useCallback((value: string) => {
    const next: QuickActionsTab = value === "office" ? "office" : "coding"
    setTab(next)
    saveQuickActionsTab(next)
  }, [])

  const handleOffice = useCallback(
    (action: OfficeAction) => {
      const label = t(action.id as Parameters<typeof t>[0])
      onSelect({
        text: t(action.promptKey as Parameters<typeof t>[0]),
        skill: { id: action.skillId, label },
      })
    },
    [onSelect, t]
  )

  const handleExpert = useCallback(
    (item: ExpertListItem) => {
      const label =
        pickLocalized(item.metadata.display_name, locale) || item.metadata.id
      // Experts are open-ended coding skills: inject just the `/id` badge and
      // let the user describe the task (no canned template like office docs).
      onSelect({ text: "", skill: { id: item.metadata.id, label } })
    },
    [onSelect, locale]
  )

  const { codingFeatured, codingRest } = useMemo(() => {
    const byId = new Map(experts.map((e) => [e.metadata.id, e]))
    const featured = CODING_FEATURED.map((f) => {
      const item = byId.get(f.id)
      return item ? { ...f, item } : null
    }).filter(
      (
        v
      ): v is (typeof CODING_FEATURED)[number] & {
        item: ExpertListItem
      } => v !== null
    )
    const rest = experts
      .filter((e) => !CODING_FEATURED_IDS.has(e.metadata.id))
      .sort(
        (a, b) =>
          (a.metadata.sort_order ?? 0) - (b.metadata.sort_order ?? 0) ||
          a.metadata.id.localeCompare(b.metadata.id)
      )
    return { codingFeatured: featured, codingRest: rest }
  }, [experts])

  return (
    <Tabs value={tab} onValueChange={handleTabChange}>
      <TabsList className="mx-auto">
        <TabsTrigger
          value="coding"
          className="data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm"
        >
          {t("tabs.coding")}
        </TabsTrigger>
        <TabsTrigger
          value="office"
          className="data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm"
        >
          {t("tabs.office")}
        </TabsTrigger>
      </TabsList>

      <TabsContent value="coding" className="flex flex-col gap-2">
        {codingFeatured.length > 0 && (
          <div className="grid grid-cols-3 gap-2">
            {codingFeatured.map((f) => (
              <BigCard
                key={f.id}
                icon={getExpertIcon(f.item.metadata.icon)}
                accent={f.accent}
                title={
                  pickLocalized(f.item.metadata.display_name, locale) || f.id
                }
                description={t(f.descKey as Parameters<typeof t>[0])}
                onClick={() => handleExpert(f.item)}
              />
            ))}
          </div>
        )}
        <Marquee itemCount={codingRest.length}>
          {(clone) =>
            codingRest.map((item) => {
              const label =
                pickLocalized(item.metadata.display_name, locale) ||
                item.metadata.id
              return (
                <SkillBar
                  key={`${item.metadata.id}-${clone ? "c" : "r"}`}
                  clone={clone}
                  icon={getExpertIcon(item.metadata.icon)}
                  label={label}
                  title={pickLocalized(item.metadata.description, locale)}
                  onClick={() => handleExpert(item)}
                />
              )
            })
          }
        </Marquee>
      </TabsContent>

      <TabsContent value="office" className="flex flex-col gap-2">
        <div className="grid grid-cols-3 gap-2">
          {OFFICE_FIXED.map((action) => (
            <BigCard
              key={action.id}
              icon={action.icon}
              accent={action.accent}
              title={t(action.id as Parameters<typeof t>[0])}
              description={t(`${action.id}Desc` as Parameters<typeof t>[0])}
              onClick={() => handleOffice(action)}
            />
          ))}
        </div>
        <Marquee itemCount={OFFICE_SCROLL.length}>
          {(clone) =>
            OFFICE_SCROLL.map((action) => (
              <SkillBar
                key={`${action.id}-${clone ? "c" : "r"}`}
                clone={clone}
                icon={action.icon}
                label={t(action.id as Parameters<typeof t>[0])}
                title={t(`${action.id}Desc` as Parameters<typeof t>[0])}
                onClick={() => handleOffice(action)}
              />
            ))
          }
        </Marquee>
      </TabsContent>
    </Tabs>
  )
}
