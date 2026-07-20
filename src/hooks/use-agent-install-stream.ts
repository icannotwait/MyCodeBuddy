import { useInstallStream } from "@/hooks/use-install-stream"
import type { AgentInstallEvent } from "@/lib/types"

const AGENT_INSTALL_EVENT = "app://agent-install"

export type AgentInstallStatus = "idle" | "running" | "success" | "failed"

export function useAgentInstallStream() {
  return useInstallStream<AgentInstallEvent>(AGENT_INSTALL_EVENT)
}
