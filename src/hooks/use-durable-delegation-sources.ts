"use client"

import { useEffect, useRef, useState } from "react"

import type { DelegationCardSource } from "@/hooks/use-delegation-card-model"
import { listChildConversations } from "@/lib/api"
import {
  childConversationToDelegationSource,
  sortChildConversationsForOverlay,
} from "@/lib/delegation-overlay-history"
import { onTransportReconnect, subscribe } from "@/lib/platform"
import {
  CONVERSATION_CHANGED_EVENT,
  type ConversationChange,
} from "@/lib/types"

const EMPTY_SOURCES: DelegationCardSource[] = []

type Snapshot = {
  conversationId: number
  sources: DelegationCardSource[]
}

function shouldRefetchChildList(
  change: ConversationChange,
  conversationId: number,
  knownChildIds: ReadonlySet<number>
): boolean {
  if (change.kind === "upsert") {
    return (
      change.summary.parent_id === conversationId ||
      change.summary.id === conversationId
    )
  }
  if (change.kind === "deleted") {
    return knownChildIds.has(change.id)
  }
  if (change.kind === "state") {
    return knownChildIds.has(change.patch.id)
  }
  return false
}

export function useDurableDelegationSources(
  conversationId: number
): DelegationCardSource[] {
  const [snapshot, setSnapshot] = useState<Snapshot>(() => ({
    conversationId,
    sources: EMPTY_SOURCES,
  }))
  const knownChildIdsRef = useRef<Set<number>>(new Set())

  useEffect(() => {
    let disposed = false
    let requestId = 0
    let unsubscribe: (() => void) | undefined
    knownChildIdsRef.current = new Set()

    const load = async () => {
      const current = ++requestId
      try {
        const children = await listChildConversations(conversationId)
        if (disposed || current !== requestId) return
        knownChildIdsRef.current = new Set(children.map((child) => child.id))
        const next = sortChildConversationsForOverlay(children).map((child) =>
          childConversationToDelegationSource(conversationId, child)
        )
        const sources = next.length === 0 ? EMPTY_SOURCES : next
        setSnapshot((current) => {
          if (
            current.conversationId === conversationId &&
            current.sources === sources
          ) {
            return current
          }
          if (
            current.conversationId === conversationId &&
            current.sources.length === 0 &&
            sources.length === 0
          ) {
            return current
          }
          return { conversationId, sources }
        })
      } catch {
        // Keep the last successful snapshot for this conversation.
      }
    }

    const startLoad = () => {
      if (!disposed) void load()
    }

    void subscribe<ConversationChange>(CONVERSATION_CHANGED_EVENT, (change) => {
      if (
        shouldRefetchChildList(change, conversationId, knownChildIdsRef.current)
      ) {
        void load()
      }
    })
      .then((off) => {
        if (disposed) {
          off()
          return
        }
        unsubscribe = off
        startLoad()
      })
      .catch(() => {
        startLoad()
      })

    const offReconnect = onTransportReconnect(() => {
      startLoad()
    })

    return () => {
      disposed = true
      unsubscribe?.()
      offReconnect?.()
    }
  }, [conversationId])

  return snapshot.conversationId === conversationId
    ? snapshot.sources
    : EMPTY_SOURCES
}
