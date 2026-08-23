"use client"

import { createContext, useContext, useMemo, type ReactNode } from "react"

export type GrokSessionImagePhase = "live" | "complete"
export type GrokSessionImageScopeValue = {
  conversationId: number
  phase: GrokSessionImagePhase
}

const GrokConversationContext = createContext<number | null>(null)
const GrokSessionImageScopeContext =
  createContext<GrokSessionImageScopeValue | null>(null)

export function GrokConversationProvider({
  conversationId,
  children,
}: {
  conversationId: number | null
  children: ReactNode
}) {
  const stableConversationId =
    typeof conversationId === "number" &&
    Number.isInteger(conversationId) &&
    conversationId > 0
      ? conversationId
      : null
  return (
    <GrokConversationContext.Provider value={stableConversationId}>
      {children}
    </GrokConversationContext.Provider>
  )
}

export function GrokSessionImageScope({
  phase,
  children,
}: {
  phase: GrokSessionImagePhase | null
  children: ReactNode
}) {
  const conversationId = useGrokConversationId()
  const value = useMemo<GrokSessionImageScopeValue | null>(
    () =>
      conversationId !== null && phase !== null
        ? { conversationId, phase }
        : null,
    [conversationId, phase]
  )
  return (
    <GrokSessionImageScopeContext.Provider value={value}>
      {children}
    </GrokSessionImageScopeContext.Provider>
  )
}

export function useGrokConversationId(): number | null {
  return useContext(GrokConversationContext)
}

export function useGrokSessionImageScope(): GrokSessionImageScopeValue | null {
  return useContext(GrokSessionImageScopeContext)
}
