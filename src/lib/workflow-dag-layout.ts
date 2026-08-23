import type { WorkflowEdgeSnapshot, WorkflowNodeSnapshot } from "@/lib/types"

export const NODE_IDEAL_WIDTH = 148
export const NODE_MIN_WIDTH = 96
export const NODE_HEIGHT = 48
export const NODE_GAP_X = 12
export const RANK_GAP_Y = 28
export const PAD_X = 8
export const PAD_Y = 8
export const ARROW_END_GAP = 8

export interface WorkflowDagLayoutInput {
  nodes: readonly WorkflowNodeSnapshot[]
  edges: readonly WorkflowEdgeSnapshot[]
  viewportWidth: number
  direction: "ltr" | "rtl"
}
export interface LaidOutNode {
  nodeIndex: number
  nodeId: string
  rank: number
  x: number
  y: number
  width: number
  height: number
}
export interface LaidOutEdge {
  edgeIndex: number
  edgeId: string | null
  from: string
  to: string
  path: string
}
export type WorkflowDagLayoutError =
  | "empty"
  | "invalid_width"
  | "invalid_node_id"
  | "duplicate_node"
  | "duplicate_edge"
  | "dangling_edge"
  | "cycle"
  | "unsupported_edge_span"
export type WorkflowDagLayoutResult =
  | {
      ok: true
      canvasWidth: number
      height: number
      nodes: LaidOutNode[]
      edges: LaidOutEdge[]
    }
  | { ok: false; error: WorkflowDagLayoutError }

function roleOrder(role: string | null | undefined): number {
  if (role === "implementer") return 0
  if (role === "author") return 1
  if (role === "reviewer") return 2
  if (role) return 3
  return 4
}
function taskOrder(taskIndex: number | null | undefined): number {
  return taskIndex == null ? Number.POSITIVE_INFINITY : taskIndex
}
function pushMinIndex(heap: number[], value: number): void {
  heap.push(value)
  let i = heap.length - 1
  while (i > 0) {
    const p = Math.floor((i - 1) / 2)
    if (heap[p] <= heap[i]) break
    ;[heap[p], heap[i]] = [heap[i], heap[p]]
    i = p
  }
}
function popMinIndex(heap: number[]): number {
  const first = heap[0]
  const last = heap.pop()!
  if (heap.length === 0) return first
  heap[0] = last
  let i = 0
  while (true) {
    const l = i * 2 + 1
    const r = l + 1
    let s = i
    if (l < heap.length && heap[l] < heap[s]) s = l
    if (r < heap.length && heap[r] < heap[s]) s = r
    if (s === i) return first
    ;[heap[i], heap[s]] = [heap[s], heap[i]]
    i = s
  }
}
function edgePath(from: LaidOutNode, to: LaidOutNode): string {
  const startX = from.x + from.width / 2
  const startY = from.y + from.height
  const endX = to.x + to.width / 2
  const endY = to.y - ARROW_END_GAP
  const middleY = (startY + endY) / 2
  return `M ${startX} ${startY} C ${startX} ${middleY}, ${endX} ${middleY}, ${endX} ${endY}`
}

export function layoutWorkflowDag({
  nodes,
  edges,
  viewportWidth,
  direction,
}: WorkflowDagLayoutInput): WorkflowDagLayoutResult {
  if (nodes.length === 0) return { ok: false, error: "empty" }
  if (!Number.isFinite(viewportWidth) || viewportWidth <= 0)
    return { ok: false, error: "invalid_width" }
  if (nodes.some((node) => !node.node_id.trim()))
    return { ok: false, error: "invalid_node_id" }
  const nodeIndexById = new Map<string, number>()
  for (let index = 0; index < nodes.length; index += 1) {
    const id = nodes[index].node_id
    if (nodeIndexById.has(id)) return { ok: false, error: "duplicate_node" }
    nodeIndexById.set(id, index)
  }
  const seenRelations = new Map<string, Set<string>>()
  for (const edge of edges) {
    const targets = seenRelations.get(edge.from) ?? new Set<string>()
    if (targets.has(edge.to)) return { ok: false, error: "duplicate_edge" }
    targets.add(edge.to)
    seenRelations.set(edge.from, targets)
  }
  const resolvedEdges: { fromIndex: number; toIndex: number }[] = []
  for (const edge of edges) {
    const fromIndex = nodeIndexById.get(edge.from)
    const toIndex = nodeIndexById.get(edge.to)
    if (fromIndex == null || toIndex == null)
      return { ok: false, error: "dangling_edge" }
    resolvedEdges.push({ fromIndex, toIndex })
  }
  const outgoing = Array.from({ length: nodes.length }, () => [] as number[])
  const indegree = Array.from({ length: nodes.length }, () => 0)
  for (const { fromIndex, toIndex } of resolvedEdges) {
    if (fromIndex === toIndex) return { ok: false, error: "cycle" }
    outgoing[fromIndex].push(toIndex)
    indegree[toIndex] += 1
  }
  const remaining = [...indegree]
  const ranks = Array.from({ length: nodes.length }, () => 0)
  const ready: number[] = []
  for (let index = 0; index < nodes.length; index += 1)
    if (remaining[index] === 0) pushMinIndex(ready, index)
  const topological: number[] = []
  while (ready.length > 0) {
    const current = popMinIndex(ready)
    topological.push(current)
    for (const successor of outgoing[current]) {
      ranks[successor] = Math.max(ranks[successor], ranks[current] + 1)
      remaining[successor] -= 1
      if (remaining[successor] === 0) pushMinIndex(ready, successor)
    }
  }
  if (topological.length !== nodes.length) return { ok: false, error: "cycle" }
  for (const { fromIndex, toIndex } of resolvedEdges)
    if (ranks[toIndex] - ranks[fromIndex] !== 1)
      return { ok: false, error: "unsupported_edge_span" }
  const rankCount = Math.max(...ranks) + 1
  const indicesByRank = Array.from({ length: rankCount }, () => [] as number[])
  for (let nodeIndex = 0; nodeIndex < nodes.length; nodeIndex += 1)
    indicesByRank[ranks[nodeIndex]].push(nodeIndex)
  for (const indices of indicesByRank)
    indices.sort((a, b) => {
      const left = nodes[a]
      const right = nodes[b]
      const task = taskOrder(left.task_index) - taskOrder(right.task_index)
      if (task !== 0) return task
      const role = roleOrder(left.role) - roleOrder(right.role)
      return role !== 0 ? role : a - b
    })
  const maxRankSize = Math.max(
    ...indicesByRank.map((indices) => indices.length)
  )
  const availablePerNode =
    (viewportWidth - 2 * PAD_X - (maxRankSize - 1) * NODE_GAP_X) / maxRankSize
  const nodeWidth = Math.min(
    NODE_IDEAL_WIDTH,
    Math.max(NODE_MIN_WIDTH, Math.floor(availablePerNode))
  )
  const requiredWidth =
    2 * PAD_X + maxRankSize * nodeWidth + (maxRankSize - 1) * NODE_GAP_X
  const canvasWidth = Math.max(viewportWidth, requiredWidth)
  const height =
    2 * PAD_Y + rankCount * NODE_HEIGHT + (rankCount - 1) * RANK_GAP_Y
  const laidOutBySourceIndex = new Map<number, LaidOutNode>()
  const laidOutNodes: LaidOutNode[] = []
  indicesByRank.forEach((indices, rank) => {
    const rowWidth =
      indices.length * nodeWidth + (indices.length - 1) * NODE_GAP_X
    const startX = (canvasWidth - rowWidth) / 2
    indices.forEach((nodeIndex, indexInRank) => {
      const ltrX = startX + indexInRank * (nodeWidth + NODE_GAP_X)
      const item = {
        nodeIndex,
        nodeId: nodes[nodeIndex].node_id,
        rank,
        x: direction === "rtl" ? canvasWidth - ltrX - nodeWidth : ltrX,
        y: PAD_Y + rank * (NODE_HEIGHT + RANK_GAP_Y),
        width: nodeWidth,
        height: NODE_HEIGHT,
      }
      laidOutBySourceIndex.set(nodeIndex, item)
      laidOutNodes.push(item)
    })
  })
  const laidOutEdges = edges.map((edge, edgeIndex) => {
    const { fromIndex, toIndex } = resolvedEdges[edgeIndex]
    const from = laidOutBySourceIndex.get(fromIndex)!
    const to = laidOutBySourceIndex.get(toIndex)!
    return {
      edgeIndex,
      edgeId: edge.id ?? null,
      from: edge.from,
      to: edge.to,
      path: edgePath(from, to),
    }
  })
  return {
    ok: true,
    canvasWidth,
    height,
    nodes: laidOutNodes,
    edges: laidOutEdges,
  }
}
