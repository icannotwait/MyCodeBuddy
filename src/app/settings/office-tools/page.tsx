"use client"

import { useEffect } from "react"
import { useRouter } from "next/navigation"

// The Office Tools settings page was merged into the unified "Skill Packs" hub.
// This stub keeps the old route working (bookmarks, in-app deep-links) by
// redirecting to the Office tab while preserving any existing query params
// (e.g. ?remoteConnectionId=N).
export default function SettingsOfficeToolsRedirect() {
  const router = useRouter()
  useEffect(() => {
    const search = typeof window !== "undefined" ? window.location.search : ""
    const params = new URLSearchParams(search)
    params.set("tab", "office")
    router.replace(`/settings/skill-packs?${params.toString()}`)
  }, [router])
  return null
}
