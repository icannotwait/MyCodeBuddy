import { useInstallStream } from "@/hooks/use-install-stream"
import type { OfficecliInstallEvent } from "@/lib/types"

const OFFICECLI_INSTALL_EVENT = "app://officecli-install"

export type OfficecliInstallStatus = "idle" | "running" | "success" | "failed"

export function useOfficecliInstallStream() {
  return useInstallStream<OfficecliInstallEvent>(OFFICECLI_INSTALL_EVENT)
}
