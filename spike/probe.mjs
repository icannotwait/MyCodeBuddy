import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import { readFileSync, writeFileSync } from "node:fs"
import os from "node:os"
import { pathToFileURL } from "node:url"

// Disposable P00 gate. Not imported by the product. It records whether a
// real probe is allowed, and it never installs crun, starts a container,
// reads credentials, or sends a model or network request.

const REQUIRED_IDS = [
  "rootless_crun_create_list_reap",
  "namespace_cgroup_isolation",
  "codex_custom_base_url",
  "endpoint_compatibility",
  "codeg_mcp_mounted_socket",
  "shell_fs_escape",
]

const CRITICAL_CHAINS = [
  {
    id: "rootless_process",
    covers: [
      "rootless_crun_create_list_reap",
      "namespace_cgroup_isolation",
    ],
  },
  {
    id: "model_gateway",
    covers: ["codex_custom_base_url"],
  },
  {
    id: "companion_injection",
    covers: ["codeg_mcp_mounted_socket"],
  },
  {
    id: "native_tool_boundary",
    covers: ["shell_fs_escape"],
  },
]

let catalogCache
let hostCache

function sha256(text) {
  return createHash("sha256").update(text, "utf8").digest("hex")
}

function catalog() {
  if (!catalogCache) {
    const raw = readFileSync(
      new URL("./fixtures/cases.json", import.meta.url),
      "utf8"
    )
    const parsed = JSON.parse(raw)
    if (!Array.isArray(parsed.cases)) {
      throw new Error("spike fixture cases must be an array")
    }
    const ids = new Set(parsed.cases.map((item) => item.id))
    for (const id of REQUIRED_IDS) {
      if (!ids.has(id)) {
        throw new Error(`spike fixture missing case ${id}`)
      }
    }
    catalogCache = parsed.cases
  }
  return catalogCache
}

function lookupCrun() {
  if (process.platform !== "win32") {
    return {
      crun_version: "unknown",
      detail: "crun was not looked up off the Windows P00 host",
    }
  }
  try {
    execFileSync("where.exe", ["crun"], {
      encoding: "utf8",
      timeout: 5000,
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
    })
    return {
      crun_version: "present_not_executed",
      detail:
        "where.exe found crun; version and sha256 were not read and crun was not started",
    }
  } catch {
    return {
      crun_version: "not_installed",
      detail: "where.exe did not find crun; it was not installed or started",
    }
  }
}

function lookupOs() {
  const fallback = {
    os_build: `${os.version()} ${os.release()}`.trim(),
    kernel: os.release(),
  }
  if (process.platform !== "win32") {
    return fallback
  }
  try {
    const output = execFileSync("cmd.exe", ["/c", "ver"], {
      encoding: "utf8",
      timeout: 5000,
      windowsHide: true,
    })
    const match = output.match(/Version\s+([0-9.]+)/i)
    const version = match ? match[1] : os.release()
    return {
      os_build: `${os.version()} ${version}`.trim(),
      kernel: version,
    }
  } catch {
    return fallback
  }
}

function hostFacts() {
  if (!hostCache) {
    const crun = lookupCrun()
    const observed = lookupOs()
    hostCache = {
      ...observed,
      ...crun,
      host_platform: process.platform,
      host_arch: process.arch,
      host_node_version: process.version,
    }
  }
  return hostCache
}

function classifyProfile(profile) {
  const missingProfile = !profile || typeof profile !== "object"
  const probeApproved = profile?.approval?.probe === true
  const modelApproved = profile?.approval?.model_request === true
  // This gate never reads candidate binaries, so it has no precise hash.
  const preciseObservedHashes = false
  const linuxCandidate = process.platform === "linux"
  return {
    missingProfile,
    probeApproved,
    modelApproved,
    preciseObservedHashes,
    linuxCandidate,
    can_probe: preciseObservedHashes && probeApproved && linuxCandidate,
  }
}

function caseBlockReason(item, host, profileState) {
  if (item.kind === "model_request" || item.id === "endpoint_compatibility") {
    return profileState.modelApproved
      ? "Endpoint compatibility is not_tested. A model_request flag was set, but this spike sent no model or network request."
      : "Endpoint compatibility is not_tested. Model-request approval is absent, and no model or network request was sent."
  }
  if (!profileState.linuxCandidate) {
    const reasons = {
      rootless_crun_create_list_reap: `blocked_platform: ${host.os_build} (${host.host_platform}/${host.host_arch}) cannot run rootless crun create/list/reap. ${host.detail}. No image was downloaded and no container was started.`,
      namespace_cgroup_isolation: `blocked_platform: ${host.host_platform} cannot prove Linux user, mount, pid, or network namespaces, or cgroup limits. The isolation probe was not executed.`,
      codex_custom_base_url: `blocked_platform: Codex custom base_url inside a rootless Linux sandbox was not executed on ${host.host_platform}. Host model credentials were not read and were not mounted.`,
      codeg_mcp_mounted_socket: `blocked_platform: codeg-mcp was not launched and no Unix socket was mounted. ${host.host_platform} is not the Linux candidate that can host that socket.`,
      shell_fs_escape: `blocked_platform: shell startup and native fs escape were not executed on ${host.host_platform}. A shell starting would not by itself be a failure; escape, egress, and tool bypass were not marked passed.`,
    }
    return (
      reasons[item.id] ||
      `blocked_platform: ${item.id} is a Linux-only chain and was not executed on ${host.host_platform}.`
    )
  }
  return `not_tested: ${item.id} was not executed. This gate does not launch crun, containers, or candidate binaries.`
}

function applyOverride(base, override) {
  const next = { ...base }
  if (!override || typeof override !== "object") return next
  if (typeof override.kind === "string") next.kind = override.kind
  if (typeof override.chain === "string") next.chain = override.chain
  if (
    typeof override.expected_rejection === "boolean" ||
    override.expected_rejection === null
  ) {
    next.expected_rejection = override.expected_rejection
  }
  return next
}

function resolveCases(cases) {
  const overrides = Array.isArray(cases) ? cases : []
  const merged = catalog().map((item) =>
    applyOverride(
      item,
      overrides.find((candidate) => candidate && candidate.id === item.id)
    )
  )
  const known = new Set(merged.map((item) => item.id))
  for (const extra of overrides) {
    if (!extra || typeof extra !== "object" || known.has(extra.id)) {
      continue
    }
    merged.push(
      applyOverride(
        {
          id: typeof extra.id === "string" ? extra.id : "unknown",
          chain: "unspecified",
          kind: "unspecified",
          critical: false,
          expected_rejection: null,
          planned_command: "",
        },
        extra
      )
    )
  }
  return merged
}

function statusFor(item, profileState) {
  if (item.kind === "model_request" || item.id === "endpoint_compatibility") {
    return "not_tested"
  }
  if (!profileState.linuxCandidate) return "blocked_platform"
  return "not_tested"
}

function buildCase(item, host, profileState) {
  const planned = typeof item.planned_command === "string"
    ? item.planned_command
    : ""
  return {
    id: item.id,
    chain: item.chain,
    kind: item.kind,
    critical: item.critical === true,
    command_sha256: null,
    planned_command_sha256: planned ? sha256(planned) : null,
    exit_status: null,
    expected_rejection: item.expected_rejection ?? null,
    actual_rejection: "not_executed",
    status: statusFor(item, profileState),
    block_reason: caseBlockReason(item, host, profileState),
  }
}

function profileBlockReason(profileState, host) {
  const reasons = []
  if (profileState.missingProfile) {
    reasons.push("missing profile")
  } else if (!profileState.preciseObservedHashes) {
    reasons.push(
      "precise candidate binary hashes were not observed; caller-supplied digests are not measurements"
    )
  }
  if (!profileState.probeApproved) {
    reasons.push("probe approval is absent")
  }
  if (!profileState.modelApproved) {
    reasons.push(
      "model request approval is absent; endpoint compatibility is not_tested"
    )
  } else {
    reasons.push(
      "model request flag is not treated as permission to send; endpoint compatibility is not_tested"
    )
  }
  if (!profileState.linuxCandidate) {
    reasons.push(
      `host ${host.os_build} (${host.host_platform}/${host.host_arch}, kernel ${host.kernel}) is not Linux x86_64/Debian 12`
    )
  }
  reasons.push(
    "model_request_count is 0; no credential was read; this result is not a qualification certificate and does not unblock G1"
  )
  return reasons.join("; ")
}

function criticalChainReport(cases) {
  return CRITICAL_CHAINS.map((chain) => {
    const covered = cases.filter((item) => chain.covers.includes(item.id))
    const verdict = covered.every(
      (item) => item.status === "blocked_platform"
    )
      ? "blocked_platform"
      : "not_tested"
    return {
      id: chain.id,
      verdict,
      covers: chain.covers,
      block_reason: covered.map((item) => item.block_reason).join(" "),
    }
  })
}

export async function probeEnvironment(profile, cases) {
  const host = hostFacts()
  const profileState = classifyProfile(profile)
  const built = resolveCases(cases).map((item) =>
    buildCase(item, host, profileState)
  )
  if (built.some((item) => item.status === "passed")) {
    throw new Error("P00 gate cannot report a chain as passed")
  }
  const criticalChains = criticalChainReport(built)
  const untested = built
    .filter((item) => item.status === "not_tested")
    .map((item) => ({ id: item.id, reason: item.block_reason }))
  const verdict = criticalChains.every(
    (chain) => chain.verdict === "blocked_platform"
  )
    ? "blocked_platform"
    : "not_tested"
  return {
    can_probe: profileState.can_probe,
    model_request_count: 0,
    qualification_issued: false,
    g1_unblocked: false,
    verdict,
    endpoint_compatibility: "not_tested",
    os_build: host.os_build,
    kernel: host.kernel,
    host_platform: host.host_platform,
    host_arch: host.host_arch,
    host_node_version: host.host_node_version,
    crun_version: host.crun_version,
    crun_sha256: "unknown",
    image_digest: "unknown",
    adapter_sha256: "unknown",
    codex_sha256: "unknown",
    node_sha256: "unknown",
    mcp_sha256: "unknown",
    block_reason: profileBlockReason(profileState, host),
    cases: built,
    untested,
    critical_chains: criticalChains,
  }
}

function isDirectRun() {
  const entry = process.argv[1]
  if (!entry) return false
  return import.meta.url === pathToFileURL(entry).href
}

if (isDirectRun()) {
  const report = await probeEnvironment(undefined, undefined)
  writeFileSync(
    new URL("./report.json", import.meta.url),
    `${JSON.stringify(report, null, 2)}\n`,
    "utf8"
  )
}
