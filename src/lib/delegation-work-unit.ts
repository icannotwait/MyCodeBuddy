import type { DelegationRunIdentity } from "@/lib/delegation-card"

export interface DelegationRunRecord<T> {
  value: T
  identity: DelegationRunIdentity
}

export interface DelegationIdentityIndex {
  taskToUnitKey: ReadonlyMap<string, string>
  workUnitToUnitKey: ReadonlyMap<string, string>
  knownTaskIds: ReadonlySet<string>
  knownWorkUnitKeys: ReadonlySet<string>
}

export interface DelegationWorkUnit<T> {
  key: string
  runs: DelegationRunRecord<T>[]
}

function identityTokens(identity: DelegationRunIdentity): string[] {
  const parent = identity.parentConversationId
  const tokens: string[] = []
  if (identity.workUnitKey) {
    tokens.push(`${parent}\u0000work\u0000${identity.workUnitKey}`)
  }
  if (identity.childConversationId != null) {
    tokens.push(`${parent}\u0000child\u0000${identity.childConversationId}`)
  }
  if (identity.taskId) {
    tokens.push(`${parent}\u0000task\u0000${identity.taskId}`)
  }
  for (const taskId of identity.linkedTaskIds) {
    if (taskId) tokens.push(`${parent}\u0000task\u0000${taskId}`)
  }
  return tokens
}

function displayKey<T>(runs: readonly DelegationRunRecord<T>[]): string {
  for (const run of runs) {
    if (run.identity.workUnitKey) return run.identity.workUnitKey
  }
  for (const run of runs) {
    const { parentConversationId, childConversationId } = run.identity
    if (childConversationId != null) {
      return `child:${parentConversationId}:${childConversationId}`
    }
  }
  for (const run of runs) {
    const { parentConversationId, taskId } = run.identity
    if (taskId) return `task:${parentConversationId}:${taskId}`
  }
  const first = runs[0].identity
  return `tool:${first.parentConversationId}:${first.parentToolUseId}`
}

function addUniqueIndex(
  map: Map<string, string>,
  ambiguous: Set<string>,
  identity: string,
  unitKey: string
): void {
  if (!identity || ambiguous.has(identity)) return
  const existing = map.get(identity)
  if (existing === undefined || existing === unitKey) {
    map.set(identity, unitKey)
    return
  }
  map.delete(identity)
  ambiguous.add(identity)
}

export function groupDelegationRuns<T>(
  records: readonly DelegationRunRecord<T>[]
): {
  units: DelegationWorkUnit<T>[]
  index: DelegationIdentityIndex
} {
  const parents = records.map((_, index) => index)
  const componentWorkKeys = records.map((record) => {
    const keys = new Set<string>()
    if (record.identity.workUnitKey) keys.add(record.identity.workUnitKey)
    return keys
  })
  const find = (index: number): number => {
    let root = index
    while (parents[root] !== root) root = parents[root]
    while (parents[index] !== index) {
      const next = parents[index]
      parents[index] = root
      index = next
    }
    return root
  }
  const union = (left: number, right: number): void => {
    const leftRoot = find(left)
    const rightRoot = find(right)
    if (leftRoot === rightRoot) return
    const leftKeys = componentWorkKeys[leftRoot]
    const rightKeys = componentWorkKeys[rightRoot]
    if (
      leftKeys.size > 0 &&
      rightKeys.size > 0 &&
      ![...leftKeys].some((key) => rightKeys.has(key))
    ) {
      return
    }
    parents[rightRoot] = leftRoot
    for (const key of rightKeys) leftKeys.add(key)
  }

  const tokenOwner = new Map<string, number>()
  records.forEach((record, index) => {
    for (const token of identityTokens(record.identity)) {
      const owner = tokenOwner.get(token)
      if (owner === undefined) tokenOwner.set(token, index)
      else union(owner, index)
    }
  })

  const rootsInOrder: number[] = []
  const runsByRoot = new Map<number, DelegationRunRecord<T>[]>()
  records.forEach((record, index) => {
    const root = find(index)
    const runs = runsByRoot.get(root)
    if (runs) runs.push(record)
    else {
      rootsInOrder.push(root)
      runsByRoot.set(root, [record])
    }
  })

  const units = rootsInOrder.map((root) => {
    const runs = runsByRoot.get(root) ?? []
    return { key: displayKey(runs), runs }
  })
  const taskToUnitKey = new Map<string, string>()
  const workUnitToUnitKey = new Map<string, string>()
  const ambiguousTaskIds = new Set<string>()
  const ambiguousWorkUnitKeys = new Set<string>()
  for (const unit of units) {
    for (const run of unit.runs) {
      if (run.identity.taskId) {
        addUniqueIndex(
          taskToUnitKey,
          ambiguousTaskIds,
          run.identity.taskId,
          unit.key
        )
      }
      for (const taskId of run.identity.linkedTaskIds) {
        addUniqueIndex(taskToUnitKey, ambiguousTaskIds, taskId, unit.key)
      }
      if (run.identity.workUnitKey) {
        addUniqueIndex(
          workUnitToUnitKey,
          ambiguousWorkUnitKeys,
          run.identity.workUnitKey,
          unit.key
        )
      }
    }
  }

  return {
    units,
    index: {
      taskToUnitKey,
      workUnitToUnitKey,
      knownTaskIds: new Set(taskToUnitKey.keys()),
      knownWorkUnitKeys: new Set(workUnitToUnitKey.keys()),
    },
  }
}
