"use client"

import Link from "next/link"
import { useId, useState } from "react"
import { useTranslations } from "next-intl"
import { ChevronDown } from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

export type RoundtableRoomListProps = {
  workspaceId: string
  roomId?: string
  rooms: Array<{ room_id: string; status: string; config: { topic: string } }>
  loading: boolean
  error: string | null
  cursor: string | null
  onRefresh: () => void
  onMore: () => void
}

export function RoundtableRoomList({
  workspaceId,
  roomId,
  rooms,
  loading,
  error,
  cursor,
  onRefresh,
  onMore,
}: RoundtableRoomListProps) {
  const t = useTranslations("Roundtable")
  const [expanded, setExpanded] = useState(false)
  const panelId = useId()
  const workspaceHref = `/roundtable?workspace_id=${encodeURIComponent(workspaceId)}`

  return (
    <aside
      aria-label={t("discussions")}
      className="min-w-0 self-start rounded-xl border bg-card p-3"
    >
      <Button
        type="button"
        variant="ghost"
        className="w-full justify-between md:hidden"
        aria-expanded={expanded}
        aria-controls={panelId}
        onClick={() => setExpanded((value) => !value)}
      >
        {t("discussions")}
        <ChevronDown
          aria-hidden="true"
          className={cn("transition-transform", expanded && "rotate-180")}
        />
      </Button>
      <h2 className="mb-3 hidden text-sm font-semibold md:block">
        {t("discussions")}
      </h2>
      <div
        id={panelId}
        className={cn(
          "min-w-0 flex-col gap-3 pt-3 md:flex md:pt-0",
          expanded ? "flex" : "hidden"
        )}
      >
        <Link
          href={workspaceHref}
          className="rounded-md px-2 py-1 text-sm font-medium [overflow-wrap:anywhere] hover:bg-muted focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
        >
          {t("new")}
        </Link>
        <Button
          type="button"
          variant="outline"
          disabled={loading}
          onClick={onRefresh}
        >
          {t("refresh")}
        </Button>
        {error ? (
          <p
            role="alert"
            className="text-sm text-destructive [overflow-wrap:anywhere]"
          >
            {error}
          </p>
        ) : null}
        {loading ? (
          <p role="status" className="text-sm text-muted-foreground">
            {t("loadingRooms")}
          </p>
        ) : null}
        {!loading && !error && rooms.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("noRooms")}</p>
        ) : null}
        <ul className="max-h-64 min-w-0 space-y-1 overflow-y-auto overscroll-contain p-1 md:max-h-[calc(100dvh-18rem)]">
          {rooms.map((room) => (
            <li key={room.room_id}>
              <Link
                href={`${workspaceHref}&room_id=${encodeURIComponent(room.room_id)}`}
                aria-current={room.room_id === roomId ? "page" : undefined}
                className={cn(
                  "block rounded-md px-2 py-2 text-sm [overflow-wrap:anywhere] hover:bg-muted focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring",
                  room.room_id === roomId && "bg-muted font-medium"
                )}
              >
                {room.config.topic}
                <span className="mt-1 block text-xs font-normal text-muted-foreground">
                  {room.status}
                </span>
              </Link>
            </li>
          ))}
        </ul>
        {cursor ? (
          <Button
            type="button"
            variant="outline"
            disabled={loading}
            onClick={onMore}
          >
            {t("more")}
          </Button>
        ) : null}
      </div>
    </aside>
  )
}
