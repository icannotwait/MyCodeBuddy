"use client"

import { useCallback, useRef, useState } from "react"
import { CircleAlert, Paperclip, X } from "lucide-react"
import { useTranslations } from "next-intl"

import type { SharedQueuedPrompt } from "@/lib/snapshot-denormalize"

interface SharedMessageQueueDisplayProps {
  queue: SharedQueuedPrompt[]
  onCancel: (queueItemId: string) => Promise<void>
  onDismissFailed: (queueItemId: string) => void
}

export function SharedMessageQueueDisplay({
  queue,
  onCancel,
  onDismissFailed,
}: SharedMessageQueueDisplayProps) {
  const t = useTranslations("Folder.chat.messageQueue")
  const [pendingCancelIds, setPendingCancelIds] = useState<Set<string>>(
    () => new Set()
  )
  const pendingCancelIdsRef = useRef(new Set<string>())

  const cancel = useCallback(
    async (queueItemId: string) => {
      if (pendingCancelIdsRef.current.has(queueItemId)) return
      pendingCancelIdsRef.current.add(queueItemId)
      setPendingCancelIds(new Set(pendingCancelIdsRef.current))
      try {
        await onCancel(queueItemId)
      } catch {
        // The lifecycle already reports transport failures. Keep the row and
        // make its cancel action available for retry.
      } finally {
        pendingCancelIdsRef.current.delete(queueItemId)
        setPendingCancelIds(new Set(pendingCancelIdsRef.current))
      }
    },
    [onCancel]
  )

  if (queue.length === 0) return null

  return (
    <div
      data-testid="shared-message-queue"
      className="flex max-h-28 flex-col gap-0.5 overflow-y-auto pb-1"
    >
      {[...queue]
        .sort((a, b) => a.enqueueSeq - b.enqueueSeq)
        .map((item) => {
          const pending = pendingCancelIds.has(item.queueItemId)
          return (
            <div
              key={item.queueItemId}
              className="flex h-6 min-w-0 items-center gap-1 border-b border-border/50 px-1.5 text-[10px] leading-none"
            >
              <span className="shrink-0 font-mono text-muted-foreground/70">
                #{item.enqueueSeq}
              </span>
              <span className="line-clamp-1 min-w-0 flex-1 text-foreground/80">
                {item.visibleText ? (
                  item.visibleText
                ) : (
                  <span className="inline-flex items-center gap-1">
                    <Paperclip className="size-2.5 shrink-0" aria-hidden />
                    <span>{item.attachmentCount}</span>
                  </span>
                )}
              </span>
              {item.state === "failed" ? (
                <span
                  role="status"
                  title={item.errorCode ?? undefined}
                  className="inline-flex min-w-0 max-w-40 shrink items-center gap-1 truncate text-destructive"
                >
                  <CircleAlert className="size-2.5 shrink-0" aria-hidden />
                  <span className="truncate">{item.errorCode}</span>
                </span>
              ) : null}
              {item.state === "queued" ? (
                <button
                  type="button"
                  onClick={() => void cancel(item.queueItemId)}
                  disabled={pending}
                  title={t("deleteItem")}
                  aria-label={t("deleteItem")}
                  className="shrink-0 p-0.5 text-muted-foreground hover:text-foreground disabled:opacity-50"
                >
                  <X className="size-2.5" aria-hidden />
                </button>
              ) : null}
              {item.state === "failed" ? (
                <button
                  type="button"
                  onClick={() => onDismissFailed(item.queueItemId)}
                  title={t("deleteItem")}
                  aria-label={t("deleteItem")}
                  className="shrink-0 p-0.5 text-muted-foreground hover:text-foreground"
                >
                  <X className="size-2.5" aria-hidden />
                </button>
              ) : null}
            </div>
          )
        })}
    </div>
  )
}
