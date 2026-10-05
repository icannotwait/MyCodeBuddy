import type { RoundtableCommand } from "@/lib/roundtable/types"

const STATUS: Record<string, number> = {
  capacity_limited: 429,
  insufficient_budget: 422,
  forbidden: 403,
  not_found: 404,
}

export function roundtableBody(command: RoundtableCommand) {
  const limit = command.pageLimit ?? 100
  if (limit < 1 || limit > 500) {
    throw new Error("page_limit")
  }
  return {
    command: command.command,
    room_id: command.roomId ?? null,
    page_limit: limit,
  }
}

export function roundtableStatus(code: string) {
  return STATUS[code] ?? 400
}
