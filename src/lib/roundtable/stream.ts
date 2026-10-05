export interface PreviewChunk {
  incarnation: number
  seq: number
}

export function coalescePreview(chunks: PreviewChunk[], currentIncarnation: number) {
  const live = chunks.filter((chunk) => chunk.incarnation === currentIncarnation)
  if (live.length === 0) return null
  return { first: live[0].seq, last: live[live.length - 1].seq }
}

export function resyncAfterReconnect(serverAccepted: string[]) {
  return { accepted: serverAccepted, preview: null as string | null }
}
