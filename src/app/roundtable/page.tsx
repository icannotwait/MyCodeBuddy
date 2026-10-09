"use client"

import { Suspense } from "react"
import { useSearchParams } from "next/navigation"
import { RoundtableWorkbench } from "@/components/roundtable/roundtable-workbench"

function RoundtableRoute() {
  const params = useSearchParams()
  const roomId = params.get("room_id") ?? undefined
  const workspaceId = params.get("workspace_id") ?? ""
  return (
    <RoundtableWorkbench
      // The create form switches workspace in place, so only rooms remount.
      key={roomId ? `${workspaceId}:${roomId}` : "new"}
      workspaceId={workspaceId}
      roomId={roomId}
    />
  )
}

export default function RoundtablePage() {
  return (
    <Suspense>
      <RoundtableRoute />
    </Suspense>
  )
}
