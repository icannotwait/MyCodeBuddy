"use client"

import { useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import {
  Archive,
  ArchiveRestore,
  ChevronDown,
  ChevronRight,
  Loader2,
  Plus,
  Trash2,
} from "lucide-react"

import {
  createLoopMemory,
  deleteLoopMemory,
  listLoopMemory,
  updateLoopMemory,
} from "@/lib/loops-api"
import type { LoopMemoryKind, LoopMemoryRow } from "@/lib/types"
import { toErrorMessage } from "@/lib/app-error"
import { useLoopResource } from "@/hooks/use-loop-resource"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { MessageResponse } from "@/components/ai-elements/message"

// The human-create dropdown stays the five semantic kinds; episodic/procedural
// are reflect-authored only (D9).
const MEMORY_KINDS: LoopMemoryKind[] = [
  "constitution",
  "constraint",
  "decision",
  "preference",
  "pitfall",
]

/** CoALA layer each memory kind belongs to — the live list groups by this. */
const LAYER_OF: Record<LoopMemoryKind, "semantic" | "episodic" | "procedural"> =
  {
    constitution: "semantic",
    constraint: "semantic",
    decision: "semantic",
    preference: "semantic",
    pitfall: "semantic",
    episodic: "episodic",
    procedural: "procedural",
  }

const LAYERS: Array<
  [
    "semantic" | "episodic" | "procedural",
    "layerSemantic" | "layerEpisodic" | "layerProcedural",
  ]
> = [
  ["semantic", "layerSemantic"],
  ["episodic", "layerEpisodic"],
  ["procedural", "layerProcedural"],
]

/**
 * Space memory: the durable constraints, decisions and pitfalls fed into every
 * issue's briefing. Each entry shows its source — `human` (curated) or `agent`
 * (proposed by a loop iteration) — and can be added, archived/restored or
 * deleted. The engine reads only `active` entries, so archiving retires an entry
 * without losing it.
 */
export function MemoryPanel({ spaceId }: { spaceId: number }) {
  const t = useTranslations("Loops.memory")
  const tKind = useTranslations("Loops.memoryKind")
  const tActor = useTranslations("Loops.actorKind")
  const tTrust = useTranslations("Loops.trustTier")
  const tCommon = useTranslations("Loops.common")
  const tToasts = useTranslations("Loops.toasts")

  const [busyId, setBusyId] = useState<number | null>(null)
  const [showSuperseded, setShowSuperseded] = useState(false)
  const [addOpen, setAddOpen] = useState(false)
  const [kind, setKind] = useState<LoopMemoryKind>("decision")
  const [title, setTitle] = useState("")
  const [content, setContent] = useState("")
  const [creating, setCreating] = useState(false)

  // Space memory, kept live by the realtime provider (agent-proposed entries
  // arrive via the iteration that wrote them, which carries this space id).
  const {
    data: items,
    loading,
    refetch,
  } = useLoopResource<LoopMemoryRow[]>(() => listLoopMemory(spaceId), {
    match: (e) => e.space_id === spaceId,
    initial: [],
    deps: [spaceId],
  })

  const create = async () => {
    if (!title.trim()) return
    setCreating(true)
    try {
      await createLoopMemory({
        spaceId,
        kind,
        title: title.trim(),
        content: content.trim(),
      })
      toast.success(tToasts("memorySaved"))
      setAddOpen(false)
      setTitle("")
      setContent("")
      setKind("decision")
      refetch()
    } catch (err) {
      toast.error(tToasts("actionFailed", { message: toErrorMessage(err) }))
    } finally {
      setCreating(false)
    }
  }

  const run = async (id: number, fn: () => Promise<void>, ok: string) => {
    setBusyId(id)
    try {
      await fn()
      toast.success(ok)
      refetch()
    } catch (err) {
      toast.error(tToasts("actionFailed", { message: toErrorMessage(err) }))
    } finally {
      setBusyId(null)
    }
  }

  const setArchived = (item: LoopMemoryRow, archived: boolean) =>
    run(
      item.id,
      () =>
        updateLoopMemory({
          spaceId,
          id: item.id,
          title: item.title,
          content: item.content,
          status: archived ? "archived" : "active",
        }),
      tToasts("memorySaved")
    )

  const remove = (item: LoopMemoryRow) =>
    run(
      item.id,
      () => deleteLoopMemory(spaceId, item.id),
      tToasts("memoryDeleted")
    )

  // One memory row. `actions` adds the archive/restore + delete controls; the
  // folded superseded section renders rows read-only — supersede reversal is a
  // P4 concern, so a superseded memory deliberately has no restore action.
  const renderMemory = (m: LoopMemoryRow, actions: boolean) => {
    const busy = busyId === m.id
    const archived = m.status === "archived"
    const dimmed = archived || m.status === "superseded"
    return (
      <li
        key={m.id}
        className={`rounded-md border p-2.5 ${dimmed ? "opacity-60" : ""}`}
      >
        <div className="flex items-center gap-1.5">
          <Badge variant="outline">{tKind(m.kind)}</Badge>
          <Badge variant={m.source === "agent" ? "secondary" : "outline"}>
            {tActor(m.source)}
          </Badge>
          <Badge variant="outline">{tTrust(m.trust_tier)}</Badge>
          {archived && <Badge variant="ghost">{t("archived")}</Badge>}
          {m.status === "superseded" && (
            <Badge variant="ghost">{t("superseded")}</Badge>
          )}
          <span className="ml-1 min-w-0 flex-1 truncate text-sm font-medium">
            {m.title}
          </span>
          {actions && (
            <>
              <Button
                size="icon"
                variant="ghost"
                className="h-7 w-7 shrink-0"
                disabled={busy}
                onClick={() => void setArchived(m, !archived)}
                aria-label={archived ? t("restore") : t("archive")}
              >
                {archived ? (
                  <ArchiveRestore className="h-3.5 w-3.5" />
                ) : (
                  <Archive className="h-3.5 w-3.5" />
                )}
              </Button>
              <Button
                size="icon"
                variant="ghost"
                className="h-7 w-7 shrink-0 text-destructive hover:text-destructive"
                disabled={busy}
                onClick={() => void remove(m)}
                aria-label={tCommon("delete")}
              >
                <Trash2 className="h-3.5 w-3.5" />
              </Button>
            </>
          )}
        </div>
        {m.summary?.trim() && (
          <p
            className="mt-1 truncate text-xs text-muted-foreground"
            title={t("summary")}
          >
            {m.summary}
          </p>
        )}
        {m.content.trim() && (
          // Rendered through the shared safe Streamdown pipeline (same as chat)
          // — no raw HTML, no dangerouslySetInnerHTML.
          <div className="mt-1 break-words text-xs text-muted-foreground">
            <MessageResponse>{m.content}</MessageResponse>
          </div>
        )}
      </li>
    )
  }

  // The full index reads only `active`/`archived`; `superseded` entries are an
  // audit trail folded away by default (engine-written, P4 reflect path).
  const live = items.filter((m) => m.status !== "superseded")
  const superseded = items.filter((m) => m.status === "superseded")

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex shrink-0 items-center justify-between px-1 pb-2">
        <span className="text-xs text-muted-foreground">{t("subtitle")}</span>
        <Button
          size="sm"
          variant="outline"
          className="h-7"
          onClick={() => setAddOpen(true)}
        >
          <Plus className="mr-1 h-3.5 w-3.5" />
          {t("add")}
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {loading ? (
          <div className="flex h-24 items-center justify-center text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
          </div>
        ) : items.length === 0 ? (
          <p className="px-1 py-6 text-center text-xs text-muted-foreground">
            {t("empty")}
          </p>
        ) : (
          <div className="space-y-3">
            {LAYERS.map(([layer, labelKey]) => {
              const rows = live.filter((m) => LAYER_OF[m.kind] === layer)
              if (rows.length === 0) return null
              return (
                <div key={layer} className="space-y-2">
                  <h3 className="px-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
                    {t(labelKey)}
                  </h3>
                  <ul className="space-y-2">
                    {rows.map((m) => renderMemory(m, true))}
                  </ul>
                </div>
              )
            })}
            {superseded.length > 0 && (
              <div className="space-y-2">
                <button
                  type="button"
                  onClick={() => setShowSuperseded((v) => !v)}
                  className="flex w-full items-center gap-1 px-1 pt-1 text-xs text-muted-foreground hover:text-foreground"
                >
                  {showSuperseded ? (
                    <ChevronDown className="h-3.5 w-3.5" />
                  ) : (
                    <ChevronRight className="h-3.5 w-3.5" />
                  )}
                  {t("supersededSection", { count: superseded.length })}
                </button>
                {showSuperseded && (
                  <ul className="space-y-2">
                    {superseded.map((m) => renderMemory(m, false))}
                  </ul>
                )}
              </div>
            )}
          </div>
        )}
      </div>

      <Dialog
        open={addOpen}
        onOpenChange={(o) => {
          if (!o) setAddOpen(false)
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t("addTitle")}</DialogTitle>
          </DialogHeader>
          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label htmlFor="memory-kind">{t("kindLabel")}</Label>
              <div id="memory-kind">
                <Select
                  value={kind}
                  onValueChange={(v) => setKind(v as LoopMemoryKind)}
                >
                  <SelectTrigger className="h-8">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {MEMORY_KINDS.map((k) => (
                      <SelectItem key={k} value={k}>
                        {tKind(k)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="memory-title">{t("titleLabel")}</Label>
              <Input
                id="memory-title"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                placeholder={t("titlePlaceholder")}
                autoFocus
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="memory-content">{t("contentLabel")}</Label>
              <Textarea
                id="memory-content"
                value={content}
                onChange={(e) => setContent(e.target.value)}
                placeholder={t("contentPlaceholder")}
                rows={4}
              />
            </div>
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              type="button"
              onClick={() => setAddOpen(false)}
              disabled={creating}
            >
              {tCommon("cancel")}
            </Button>
            <Button
              type="button"
              onClick={create}
              disabled={creating || !title.trim()}
            >
              {creating && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {t("create")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
