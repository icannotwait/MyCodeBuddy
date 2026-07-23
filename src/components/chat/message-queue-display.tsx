"use client"

import { useCallback, type PointerEvent } from "react"
import { Reorder, useDragControls } from "motion/react"
import { GripVertical, Pencil, Play, X } from "lucide-react"
import { useTranslations } from "next-intl"
import { cn } from "@/lib/utils"
import type { QueuedMessage } from "@/hooks/use-message-queue"

interface MessageQueueDisplayProps {
  queue: QueuedMessage[]
  onReorder: (items: QueuedMessage[]) => void
  onEdit: (id: string) => void
  onDelete: (id: string) => void
  editingItemId: string | null
  /** When true and the queue is non-empty, show terminal-pause status + Resume. */
  paused?: boolean
  onResumeQueue?: () => void
}

interface QueueItemProps {
  item: QueuedMessage
  index: number
  isEditing: boolean
  onEdit: (id: string) => void
  onDelete: (id: string) => void
}

function QueueItem({
  item,
  index,
  isEditing,
  onEdit,
  onDelete,
}: QueueItemProps) {
  const t = useTranslations("Folder.chat.messageQueue")
  const dragControls = useDragControls()

  const startDrag = useCallback(
    (event: PointerEvent<HTMLButtonElement>) => {
      event.preventDefault()
      event.stopPropagation()
      dragControls.start(event)
    },
    [dragControls]
  )

  return (
    <Reorder.Item
      as="div"
      value={item}
      dragListener={false}
      dragControls={dragControls}
      className={cn(
        "flex items-center gap-1 rounded-md border px-1.5 py-1 text-[10px] leading-none select-none [text-box-trim:both] [text-box-edge:cap_alphabetic]",
        "bg-muted/40 border-border/70",
        isEditing && "border-primary/50 bg-primary/5"
      )}
    >
      <button
        type="button"
        className="shrink-0 cursor-grab touch-none active:cursor-grabbing p-0"
        onPointerDown={startDrag}
      >
        <GripVertical className="h-3 w-3 text-muted-foreground/60" />
      </button>
      <span className="shrink-0 font-mono text-[10px] text-muted-foreground/70">
        #{index + 1}
      </span>
      <span className="min-w-0 flex-1 truncate text-[10px] text-foreground/80">
        {item.draft.displayText}
      </span>
      <button
        type="button"
        onClick={() => onEdit(item.id)}
        className="shrink-0 rounded-sm p-0.5 hover:bg-muted-foreground/15 text-muted-foreground"
        title={t("editItem")}
      >
        <Pencil className="h-2.5 w-2.5" />
      </button>
      <button
        type="button"
        onClick={() => onDelete(item.id)}
        className="shrink-0 rounded-sm p-0.5 hover:bg-muted-foreground/15 text-muted-foreground"
        title={t("deleteItem")}
      >
        <X className="h-2.5 w-2.5" />
      </button>
    </Reorder.Item>
  )
}

export function MessageQueueDisplay({
  queue,
  onReorder,
  onEdit,
  onDelete,
  editingItemId,
  paused = false,
  onResumeQueue,
}: MessageQueueDisplayProps) {
  const t = useTranslations("Folder.chat.messageQueue")

  if (queue.length === 0) return null

  return (
    <div className="max-h-28 overflow-y-auto pb-1">
      {paused ? (
        <div className="mb-0.5 flex items-center justify-between gap-2 px-0.5 text-[10px] text-muted-foreground">
          <span className="min-w-0 truncate">{t("paused")}</span>
          {onResumeQueue ? (
            <button
              type="button"
              onClick={onResumeQueue}
              className="inline-flex shrink-0 items-center gap-0.5 rounded-sm px-1 py-0.5 text-[10px] text-foreground/80 hover:bg-muted-foreground/15"
              title={t("resumeQueue")}
            >
              <Play className="h-2.5 w-2.5" aria-hidden />
              {t("resumeQueue")}
            </button>
          ) : null}
        </div>
      ) : null}
      <Reorder.Group
        as="div"
        axis="y"
        values={queue}
        onReorder={onReorder}
        className="flex flex-col gap-0.5"
      >
        {queue.map((item, index) => (
          <QueueItem
            key={item.id}
            item={item}
            index={index}
            isEditing={editingItemId === item.id}
            onEdit={onEdit}
            onDelete={onDelete}
          />
        ))}
      </Reorder.Group>
    </div>
  )
}
