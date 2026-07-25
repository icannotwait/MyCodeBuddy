import { extractAppCommandError } from "@/lib/app-error"
import type { DelegateAccessState } from "@/lib/types"

export const UNKNOWN_DELEGATE_ACCESS: DelegateAccessState = {
  mode: "viewer_only",
  reason: "state_unknown",
  parent_id: null,
}

export const NON_DELEGATE_ACCESS: DelegateAccessState = {
  mode: "interactive",
  reason: null,
  parent_id: null,
}

export function isDelegateViewerOnlyRejection(error: unknown): boolean {
  return extractAppCommandError(error)?.code === "delegate_viewer_only"
}
