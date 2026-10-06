export type RoundtableStatus =
  | "draft"
  | "ready"
  | "running"
  | "pausing"
  | "paused"
  | "recovering"
  | "stopping"
  | "stopped"
  | "failed"
  | "catching_up"
  | "completed"

export interface RoundtableEvent {
  seq: number
  kind: "preview" | "preview_reset" | "accepted" | "projection" | "gap"
  text?: string
  incarnation?: string | number
  attemptId?: string
  snapshot?: RoundtableSnapshot
}

/** A verified immutable projection at the supplied durable watermark. */
export interface RoundtableSnapshot {
  accepted: string[]
  highWaterSeq: number
  status: RoundtableStatus
  closedAttemptIds?: string[]
}

export interface RoundtableView {
  status: RoundtableStatus
  accepted: string[]
  preview: string | null
  catchingUp: boolean
  lastSeq: number
  previewIncarnation: string | number | null
  previewAttemptId: string | null
  previewSeq: number
  previewClosed: boolean
  closedPreviewKeys: string[]
}

export interface RoundtableCommand {
  command: string
  roomId?: string
  pageLimit?: number
}

export interface RoundtableConfig {
  schema_version: 1
  display_name?: string
  topic: string
  workspace_id: string
  source_refs: { snapshot_id: string; base_commit: string | null }[]
  participants: {
    ordinal: number
    role: string
    provider_ref: string
    model?: string
    effort?: string
    /** `codex`, `grok`, `cursor`, or `antigravity`. Absent means Codex. */
    agent?: string
  }[]
  moderator_ordinal: number
  strategy: { type: "phased_rounds"; version: 1; critique_rounds: number }
  concurrency: number
  strict_snapshot_v1: true
  budgets: { room_budget: string; phase_budget: string }
  timeouts: { attempt_timeout: string }
  quotas: {
    output_byte_limit: number
    input_byte_limit: number
    interjection_byte_limit: number
  }
}

export interface RoundtableProjection {
  projection_ref: { id: string; hash: string }
  body: {
    schema_version: number
    room_id: string
    revision: string
    run_epoch: string
    last_seq: string
    status: RoundtableStatus
    blocked_reason: string | null
    messages: { message_id: string; hash: string }[]
    phase_refs: { phase_id: string; revision: string; state: string }[]
    replay: {
      config: RoundtableConfig | null
      speakers: {
        speaker_id: string
        ordinal: number
        role: string
        provider_ref: string
        model_id: string
      }[]
      attempts: { attempt_id: string; state: string; turn_id: string }[]
      turns: {
        turn_id: string
        phase_id: string
        speaker_id: string
        accepted_attempt_id: string | null
      }[]
      message_memberships: {
        message_id: string
        visibility: "staged" | "published" | "void"
      }[]
      evidence: {
        evidence_id: string
        content_hash: string
        body_hash: string
        published_seq: string | null
      }[]
      result_quality: string | null
      [key: string]: unknown
    }
    [key: string]: unknown
  }
}

export interface RoundtablePreviewFrame {
  subscription_id: string
  room_id: string
  speaker_id: string
  attempt_id: string
  incarnation: string
  run_epoch: string
  phase_revision: string
  chunk_seq?: string
  first_chunk_seq?: string
  last_chunk_seq?: string
  reset_baseline_seq?: string
  text: string
}

export interface RoundtableMessage {
  message_id: string
  body_hash: string
  visibility: "staged" | "published" | "void"
  body: {
    summary?: string
    recommendation?: { text?: string }
    [key: string]: unknown
  }
}

export interface RoundtableRecipient {
  ordinal: number
  provider_ref: string
  model: string
  origin: string
  agent: string
  effort: string | null
}

export interface RoundtableSourceEntry {
  path: string
  size: number
  content_hash: string
  text_admissible: boolean
  object: { object_id: string; content_hash: string; total_bytes: number }
}

export interface RoundtableSourceManifest {
  hash: string
  manifest: { manifest_id: string; entries: RoundtableSourceEntry[] }
}

export interface RoundtablePreflight {
  enabled: boolean
  readiness: string
  confirmed_preflight_id: string | null
  config_hash: string
  tools: string[]
  network: string
  writes: string
  error: unknown
  capability: { recipients: RoundtableRecipient[] } | null
  source_manifests: RoundtableSourceManifest[]
}

export interface RoundtableEvidence {
  body: {
    file_alias: string
    excerpt: string
    excerpt_hash: string
    verified: boolean
    origin: string
    line_start: number | null
    line_end: number | null
    [key: string]: unknown
  }
  hash: string
}

export interface RoundtableOperation {
  operation_id: string
  kind: string
  step: string
  status: string
  blocked_reason: string | null
}

export interface RoundtableUsage {
  usage_version: string
  measurements: { key: string; value: number }[]
  totals: { key: string; value: number }[]
  unknown_count: number
  confirmed_output_tokens: number | null
  unknown_total: boolean
  uncertain: boolean
}
