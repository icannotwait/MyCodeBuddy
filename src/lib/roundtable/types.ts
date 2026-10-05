export type RoundtableStatus =
  | "draft"
  | "running"
  | "pausing"
  | "paused"
  | "catching_up"
  | "completed"

export interface RoundtableEvent {
  seq: number
  kind: "preview" | "accepted" | "gap"
  text?: string
}

export interface RoundtableView {
  status: RoundtableStatus
  accepted: string[]
  preview: string | null
  catchingUp: boolean
}

export interface RoundtableCommand {
  command: string
  roomId?: string
  pageLimit?: number
}
