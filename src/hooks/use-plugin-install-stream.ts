import { useInstallStream } from "@/hooks/use-install-stream"
import type { PluginInstallEvent } from "@/lib/types"

const PLUGIN_INSTALL_EVENT = "app://opencode-plugin-install"

export type PluginInstallStatus = "idle" | "running" | "success" | "failed"

export function usePluginInstallStream() {
  return useInstallStream<PluginInstallEvent>(PLUGIN_INSTALL_EVENT)
}
