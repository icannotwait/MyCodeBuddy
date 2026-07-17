"use client"

import { useCallback } from "react"
import { FileSearch } from "lucide-react"
import { useOpenLinkOrFile } from "@/components/ai-elements/link-safety"
import type { UserResourceDisplay } from "@/lib/adapters/ai-elements-adapter"
import { cn } from "@/lib/utils"

interface UserResourceLinksProps {
  resources: UserResourceDisplay[]
  className?: string
}

/**
 * The attachment summary row shown beneath a user message: one grey chip per
 * attached file. Complements the inline file badges kept in message prose
 * (markdown-link → ReferenceBadge). Left-click opens the same workspace file
 * panel path as those badges (`useOpenLinkOrFile` → `openFilePreview`). Images
 * are handled separately as thumbnails.
 */
export function UserResourceLinks({
  resources,
  className,
}: UserResourceLinksProps) {
  const openLinkOrFile = useOpenLinkOrFile()

  const handleOpen = useCallback(
    (uri: string) => {
      void openLinkOrFile(uri)
    },
    [openLinkOrFile]
  )

  if (resources.length === 0) return null

  return (
    <div className={className}>
      <div className="flex flex-wrap gap-1.5">
        {resources.map((resource, index) => (
          <button
            key={`${resource.uri}-${index}`}
            type="button"
            title={resource.uri}
            onClick={() => handleOpen(resource.uri)}
            className={cn(
              "inline-flex max-w-full items-center gap-1 rounded-full border border-border/70 bg-muted/40 px-2 py-1 text-xs text-muted-foreground",
              "cursor-pointer appearance-none transition-colors hover:bg-muted/70 hover:text-foreground",
              "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
            )}
          >
            <FileSearch className="h-3 w-3 shrink-0" />
            <span className="max-w-56 truncate">{resource.name}</span>
          </button>
        ))}
      </div>
    </div>
  )
}
