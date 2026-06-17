"use client"

import { useMemo } from "react"
import { useTranslations } from "next-intl"

import type {
  LoopArtifactKind,
  LoopArtifactRow,
  LoopIterationRow,
  LoopStage,
} from "@/lib/types"
import { ArtifactStatusBadge } from "@/components/loops/issue-badges"

// One read-only column per artifact kind (the issue root is the container, not a
// board card). Order mirrors the write pipeline.
const COLUMNS: LoopArtifactKind[] = [
  "requirement",
  "design",
  "task",
  "review",
  "result",
  "reflection",
]

// Which board column an in-flight iteration's stage maps to (real-time ghosts).
// `triage` targets the issue root (no board column) and `implement` works on a
// task card that already exists (it shows its own progress — a ghost would just
// duplicate it), so neither yields a ghost. `review` DOES: its review artifact
// only lands when the review settles, so the ghost is the sole in-flight signal
// in the review column. (The graph folds reviews into task clusters, so it omits
// review there; the board has a dedicated review column, hence the difference.)
const STAGE_COLUMN: Partial<Record<LoopStage, LoopArtifactKind>> = {
  refine: "requirement",
  design: "design",
  plan: "task",
  review: "review",
  finalize: "result",
  reflect: "reflection",
}

/**
 * Read-only kanban of an issue's artifacts, one column per kind. A filter view,
 * not a workflow tool — there is no drag-and-drop; the engine owns every status
 * transition. Clicking a card opens it in the drawer via `onSelect`.
 */
export function BoardView({
  artifacts,
  liveIterations,
  onSelect,
}: {
  artifacts: LoopArtifactRow[]
  /** queued|running iterations — drives in-flight ghost cards per column. */
  liveIterations: LoopIterationRow[]
  onSelect: (id: number) => void
}) {
  const t = useTranslations("Loops.boardView")
  const tKind = useTranslations("Loops.artifactKind")
  const tStage = useTranslations("Loops.stage")
  const tDag = useTranslations("Loops.dag")

  const byKind = useMemo(() => {
    const map = new Map<LoopArtifactKind, LoopArtifactRow[]>()
    for (const a of artifacts) {
      if (a.kind === "issue") continue
      const list = map.get(a.kind) ?? []
      list.push(a)
      map.set(a.kind, list)
    }
    for (const list of map.values())
      list.sort((a, b) => a.sort - b.sort || a.id - b.id)
    return map
  }, [artifacts])

  // Suppress a ghost once THIS iteration's artifact has landed (the stale window
  // between settle and the next fetch) — mirrors the graph's producer dedup so a
  // just-landed artifact and its ghost never both show. Built over the full list:
  // provenance is independent of any display filter.
  const landedIterationIds = useMemo(
    () =>
      new Set(
        artifacts
          .map((a) => a.produced_by_iteration_id)
          .filter((x): x is number => x != null)
      ),
    [artifacts]
  )

  // In-flight iterations grouped into the column their stage targets.
  const ghostsByKind = useMemo(() => {
    const map = new Map<LoopArtifactKind, LoopIterationRow[]>()
    for (const it of liveIterations) {
      if (it.status !== "queued" && it.status !== "running") continue
      if (landedIterationIds.has(it.id)) continue
      const kind = STAGE_COLUMN[it.stage]
      if (!kind) continue
      const list = map.get(kind) ?? []
      list.push(it)
      map.set(kind, list)
    }
    return map
  }, [liveIterations, landedIterationIds])

  const total = useMemo(
    () => artifacts.filter((a) => a.kind !== "issue").length,
    [artifacts]
  )
  const ghostTotal = useMemo(
    () => [...ghostsByKind.values()].reduce((n, l) => n + l.length, 0),
    [ghostsByKind]
  )

  if (total === 0 && ghostTotal === 0) {
    return (
      <p className="px-1 py-6 text-center text-xs text-muted-foreground">
        {t("empty")}
      </p>
    )
  }

  return (
    <div className="flex gap-3 overflow-x-auto pb-2">
      {COLUMNS.map((kind) => {
        const cards = byKind.get(kind) ?? []
        const ghosts = ghostsByKind.get(kind) ?? []
        return (
          <div key={kind} className="flex w-56 shrink-0 flex-col">
            <div className="mb-1.5 flex items-center justify-between px-1">
              <span className="text-xs font-medium">{tKind(kind)}</span>
              <span className="flex items-center gap-1.5 font-mono text-[11px] text-muted-foreground">
                {ghosts.length > 0 && (
                  <span className="inline-flex items-center gap-1 text-sky-600 dark:text-sky-400">
                    <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-sky-500" />
                    {ghosts.length}
                  </span>
                )}
                {cards.length}
              </span>
            </div>
            <div className="space-y-1.5">
              {ghosts.map((g) => (
                <div
                  key={`ghost:${g.id}`}
                  className="flex w-full flex-col gap-1 rounded-md border border-dashed bg-card/60 p-2 text-left"
                >
                  <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-sky-500" />
                    {tStage(g.stage)}
                  </span>
                  <span className="text-[10px] text-muted-foreground">
                    {g.status === "running" ? tDag("running") : tDag("queued")}
                  </span>
                </div>
              ))}
              {cards.map((a) => (
                <button
                  key={a.id}
                  type="button"
                  onClick={() => onSelect(a.id)}
                  className="flex w-full flex-col gap-1.5 rounded-md border bg-card p-2 text-left hover:bg-accent"
                >
                  <span className="line-clamp-2 text-xs">{a.title}</span>
                  <div className="flex items-center gap-1.5">
                    <span className="font-mono text-[10px] text-muted-foreground">
                      #{a.issue_seq}
                    </span>
                    <ArtifactStatusBadge status={a.status} />
                  </div>
                </button>
              ))}
            </div>
          </div>
        )
      })}
    </div>
  )
}
