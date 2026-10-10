"use client"

import Link from "next/link"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { listAllFolderDetails, listModelProviders } from "@/lib/api"
import { getTransport } from "@/lib/transport"
import { initialRoundtableView } from "@/lib/roundtable/reducer"
import { applyRoundtablePreview } from "@/lib/roundtable/stream"
import {
  applyRoundtableLive,
  liveTranscriptInputs,
  type RoundtableLiveState,
} from "@/lib/roundtable/live"
import {
  loadRoundtable,
  loadRoundtableEvidence,
  loadRoundtableSource,
  roundtableCall,
  roundtableError,
  roundtableHash,
  verifyRoundtableReplay,
} from "@/lib/roundtable/api"
import type { RoundtableCommandName } from "@/lib/roundtable/api"
import {
  defaultRoundtableAgents,
  loadRoundtableAgents,
  nextRoundtableAgent,
  statusByAgent,
  type RoundtableAgentStatus,
} from "@/lib/roundtable/agents"
import {
  prepareRoundtableMutation,
  isDefinitiveRoundtableRejection,
  pendingPaidMutation,
  rememberPendingPaidMutation,
  clearPendingPaidMutation,
} from "@/lib/roundtable/mutation"
import type { PendingRoundtableMutation } from "@/lib/roundtable/mutation"
import type { FolderDetail, ModelProviderInfo } from "@/lib/types"
import type {
  RoundtableConfig,
  RoundtablePreflight,
  RoundtableProjection,
  RoundtableEvidence,
  RoundtableOperation,
  RoundtableUsage,
  RoundtableView,
} from "@/lib/roundtable/types"
import {
  CircleCheck,
  FolderOpen,
  Loader2,
  ShieldCheck,
  TriangleAlert,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { PreflightConfirmation } from "./preflight-confirmation"
import {
  ROUNDTABLE_MAX_MEMBERS,
  ROUNDTABLE_MIN_MEMBERS,
  RoundtableComposer,
  RoundtableStep,
  roundtableAgentLabel,
  seatErrorMessage,
  type ComposerMember,
} from "./roundtable-composer"
import { RoundtableSafeContent } from "./roundtable-safe-content"
import { RoundtableRoomList } from "./roundtable-room-list"
import { RoundtableWorkspaceSelect } from "./roundtable-workspace-select"
import { brandStyle, roundtableSeatBrands } from "@/lib/roundtable/brand"
import { cn } from "@/lib/utils"
import { RoundtableInlineText } from "./roundtable-inline-text"
import {
  BRAND_CLASSES,
  RoundtableSpeakerAvatar,
  RoundtableTranscript,
  speakerSeatBrand,
} from "./roundtable-transcript"
import { RoundtableConclusionActions } from "./roundtable-conclusion-actions"

type LoadedRoom = Awaited<ReturnType<typeof loadRoundtable>>
type RoomSummary = { room_id: string; status: string; config: RoundtableConfig }

/** Per-attempt limit for proposal and critique turns (`timeouts.attempt_timeout`). */
const MEMBER_ATTEMPT_TIMEOUT_MS = 225_000
/**
 * Room-budget allowance for the synthesis phase. Synthesis has no attempt or
 * phase limit of its own; it is bounded by the room budget, pause and stop.
 */
const SYNTHESIS_ROOM_ALLOWANCE_MS = 450_000
export function RoundtableWorkbench({
  workspaceId: urlWorkspaceId,
  roomId,
}: {
  workspaceId: string
  roomId?: string
}) {
  const t = useTranslations("Roundtable")
  // The create flow can switch workspace in place (keeping the form); an open
  // room stays bound to the workspace in its URL.
  const [workspaceId, setWorkspaceId] = useState(urlWorkspaceId)
  useEffect(() => setWorkspaceId(urlWorkspaceId), [urlWorkspaceId])
  const [workspaces, setWorkspaces] = useState<FolderDetail[] | null>(null)
  const [workspacesLoading, setWorkspacesLoading] = useState(true)
  const [workspacesError, setWorkspacesError] = useState<string | null>(null)
  const workspacesGeneration = useRef(0)
  const [providers, setProviders] = useState<ModelProviderInfo[]>([])
  const [rooms, setRooms] = useState<RoomSummary[]>([])
  const [listCursor, setListCursor] = useState<string | null>(null)
  const [listLoading, setListLoading] = useState(!!workspaceId)
  const [listError, setListError] = useState<string | null>(null)
  const listInFlight = useRef(false)
  const listGeneration = useRef(0)
  const [providersLoading, setProvidersLoading] = useState(true)
  const [providersError, setProvidersError] = useState<string | null>(null)
  const providersInFlight = useRef(false)
  const providersGeneration = useRef(0)
  const [loaded, setLoaded] = useState<LoadedRoom | null>(null)
  const [roomLoading, setRoomLoading] = useState(!!roomId)
  const [roomError, setRoomError] = useState<string | null>(null)
  const [streamError, setStreamError] = useState<string | null>(null)
  const [topic, setTopic] = useState("")
  const [selectedSourcePaths, setSelectedSourcePaths] = useState("")
  const [sourcePreviews, setSourcePreviews] = useState<Record<string, string>>(
    {}
  )
  // Default seats: the first two qualified agents once `roundtable_agents`
  // answers (Grok + Antigravity until then). Members can switch agents or add
  // seats before the readiness check.
  const [roles, setRoles] = useState(["", ""])
  const [agents, setAgents] = useState<string[]>(["grok", "antigravity"])
  const [agentRows, setAgentRows] = useState<RoundtableAgentStatus[] | null>(
    null
  )
  const [agentsLoading, setAgentsLoading] = useState(false)
  const agentsGeneration = useRef(0)
  // Defaults apply once, and never after the user picked an agent.
  const agentsTouched = useRef(false)
  // Empty id = the agent's qualified default binding (`provider:<agent>`).
  const [providerIds, setProviderIds] = useState<string[]>([])
  const [rounds, setRounds] = useState(2)
  const [concurrency, setConcurrency] = useState(2)
  const [moderator, setModerator] = useState(0)
  const [preflight, setPreflight] = useState<RoundtablePreflight | null>(null)
  const [confirmed, setConfirmed] = useState(false)
  const [preflightKey, setPreflightKey] = useState<string | null>(null)
  const preflightGeneration = useRef(0)
  const [recoveryConsent, setRecoveryConsent] = useState(false)
  const [editingDraft, setEditingDraft] = useState(false)
  const [evidence, setEvidence] = useState<Record<string, RoundtableEvidence>>(
    {}
  )
  const [busy, setBusy] = useState(false)
  const [checking, setChecking] = useState(false)
  const actionInFlight = useRef(false)
  const [error, setError] = useState<string | null>(null)
  const [interjection, setInterjection] = useState("")
  const [interjectMode, setInterjectMode] = useState("next_phase")
  const [refresh, setRefresh] = useState(0)
  const [operation, setOperation] = useState<RoundtableOperation | null>(null)
  const [usage, setUsage] = useState<RoundtableUsage | null>(null)
  const [previews, setPreviews] = useState<Record<string, RoundtableView>>({})
  // Display-only live output; never verified, never part of the projection.
  const [liveOutput, setLiveOutput] = useState<RoundtableLiveState>({})
  const mutationScope = JSON.stringify([workspaceId, roomId])
  const mutation = useRef<PendingRoundtableMutation | null>(
    pendingPaidMutation(mutationScope)
  )
  const [uncertainPaid, setUncertainPaid] = useState(
    () => !!pendingPaidMutation(mutationScope)
  )
  const lastScope = useRef(mutationScope)
  useEffect(() => {
    if (lastScope.current === mutationScope) return
    lastScope.current = mutationScope
    mutation.current = pendingPaidMutation(mutationScope)
    setUncertainPaid(!!mutation.current)
  }, [mutationScope])

  const describeError = useCallback(
    (error: unknown) => seatErrorMessage(error, t) ?? roundtableError(error),
    [t]
  )
  const run = useCallback(
    async (action: () => Promise<void>) => {
      if (actionInFlight.current) return
      actionInFlight.current = true
      setBusy(true)
      setError(null)
      try {
        await action()
      } catch (error) {
        setError(describeError(error))
      } finally {
        actionInFlight.current = false
        setBusy(false)
      }
    },
    [describeError]
  )

  const listRooms = useCallback(
    async (cursor?: string) => {
      if (!workspaceId || listInFlight.current) return
      listInFlight.current = true
      const generation = ++listGeneration.current
      setListLoading(true)
      setListError(null)
      try {
        const page = await roundtableCall<{
          rooms: RoomSummary[]
          cursor: string | null
        }>("roundtable_list", {
          workspace_id: workspaceId,
          limit: 100,
          ...(cursor ? { cursor } : {}),
        })
        if (generation !== listGeneration.current) return
        setRooms((previous) =>
          Array.from(
            new Map(
              [...(cursor ? previous : []), ...page.rooms].map((room) => [
                room.room_id,
                room,
              ])
            ).values()
          )
        )
        setListCursor(page.cursor)
      } catch (error) {
        if (generation === listGeneration.current)
          setListError(roundtableError(error))
      } finally {
        if (generation === listGeneration.current) {
          listInFlight.current = false
          setListLoading(false)
        }
      }
    },
    [workspaceId]
  )

  useEffect(() => {
    setRooms([])
    setListCursor(null)
    setListError(null)
  }, [workspaceId])

  useEffect(() => {
    void listRooms()
    return () => {
      listGeneration.current += 1
      listInFlight.current = false
    }
  }, [listRooms])

  const loadProviders = useCallback(async () => {
    if (providersInFlight.current) return
    providersInFlight.current = true
    const generation = ++providersGeneration.current
    setProvidersLoading(true)
    setProvidersError(null)
    try {
      const items = await listModelProviders()
      if (generation === providersGeneration.current) setProviders(items)
    } catch (error) {
      if (generation === providersGeneration.current)
        setProvidersError(roundtableError(error))
    } finally {
      if (generation === providersGeneration.current) {
        providersInFlight.current = false
        setProvidersLoading(false)
      }
    }
  }, [])

  const loadWorkspaces = useCallback(async () => {
    const generation = ++workspacesGeneration.current
    setWorkspacesLoading(true)
    setWorkspacesError(null)
    try {
      const items = await listAllFolderDetails()
      // Rooms resolve sources only against live regular folders.
      if (generation === workspacesGeneration.current)
        setWorkspaces(items.filter((folder) => folder.kind === "regular"))
    } catch (error) {
      if (generation === workspacesGeneration.current)
        setWorkspacesError(roundtableError(error))
    } finally {
      if (generation === workspacesGeneration.current)
        setWorkspacesLoading(false)
    }
  }, [])

  useEffect(() => {
    void loadWorkspaces()
    return () => {
      workspacesGeneration.current += 1
    }
  }, [loadWorkspaces])

  useEffect(() => {
    void loadProviders()
    return () => {
      providersGeneration.current += 1
      providersInFlight.current = false
    }
  }, [loadProviders])

  // Read-only agent availability (enabled, installed, credential presence,
  // qualification). A failure keeps every chip selectable; preflight decides.
  const loadAgents = useCallback(async () => {
    const generation = ++agentsGeneration.current
    setAgentsLoading(true)
    try {
      const result = await loadRoundtableAgents()
      if (generation !== agentsGeneration.current) return
      setAgentRows(result.agents)
      if (!agentsTouched.current && !roomId) {
        agentsTouched.current = true
        const picks = defaultRoundtableAgents(statusByAgent(result.agents))
        setAgents((current) =>
          current.map((agent, index) => picks[index] ?? agent)
        )
      }
    } catch {
      if (generation === agentsGeneration.current) setAgentRows(null)
    } finally {
      if (generation === agentsGeneration.current) setAgentsLoading(false)
    }
  }, [roomId])

  useEffect(() => {
    void loadAgents()
    return () => {
      agentsGeneration.current += 1
    }
  }, [loadAgents])
  const agentStatus = useMemo(() => statusByAgent(agentRows), [agentRows])

  useEffect(() => {
    if (!roomId) return
    let live = true
    let syncing = false
    let previous: RoundtableProjection | null = null
    let unsubscribe: (() => void) | undefined
    setRoomLoading(true)
    setRoomError(null)
    setStreamError(null)
    setLiveOutput({})
    let liveState: RoundtableLiveState = {}
    let resyncing = false
    const transport = getTransport()
    const subscriptionId = crypto.randomUUID()
    const attach = () =>
      roundtableCall<{ projection: RoundtableProjection }>(
        "roundtable_attach",
        {
          room_id: roomId,
          subscription_id: subscriptionId,
          protocol_version: 1,
          ...(previous
            ? {
                since_seq: previous.body.last_seq,
                projection_hash: previous.projection_ref.hash,
              }
            : {}),
        }
      )
    const sync = async () => {
      if (!live || syncing) return
      syncing = true
      try {
        const latest = await loadRoundtable(roomId)
        if (previous) await verifyRoundtableReplay(previous, latest.projection)
        if (live) {
          const active = new Set(
            latest.projection.body.replay.attempts
              .filter((attempt) =>
                ["admitted", "streaming", "validating"].includes(attempt.state)
              )
              .map((attempt) => attempt.attempt_id)
          )
          setPreviews((current) =>
            Object.fromEntries(
              Object.entries(current).filter(([attemptId]) =>
                active.has(attemptId)
              )
            )
          )
          previous = latest.projection
          setLoaded(latest)
          setRoomError(null)
        }
      } catch (error) {
        previous = null
        if (live) {
          setPreviews({})
          setRoomError(roundtableError(error))
        }
      } finally {
        syncing = false
        if (live) setRoomLoading(false)
      }
    }
    const reconnect = transport.onReconnect?.(() => {
      if (!live) return
      previous = null
      setPreviews({})
      // A fresh attach resends every live buffer in full.
      liveState = {}
      setLiveOutput({})
      void sync()
      void attach()
        .then(() => {
          if (live) setStreamError(null)
        })
        .catch((error) => {
          if (live) setStreamError(roundtableError(error))
        })
    })
    void (async () => {
      unsubscribe = await transport.subscribe(
        `roundtable://${subscriptionId}`,
        (payload: unknown) => {
          if (!live) return
          if (
            payload &&
            typeof payload === "object" &&
            "type" in payload &&
            payload.type === "roundtable_live"
          ) {
            const applied = applyRoundtableLive(liveState, payload, roomId)
            liveState = applied.state
            setLiveOutput(applied.state)
            if (applied.resync && !resyncing) {
              // A delta did not line up: re-attach for whole buffers.
              resyncing = true
              liveState = {}
              setLiveOutput({})
              void attach()
                .catch(() => undefined)
                .finally(() => {
                  resyncing = false
                })
            }
            return
          }
          if (
            previous &&
            payload &&
            typeof payload === "object" &&
            "attempt_id" in payload &&
            typeof payload.attempt_id === "string"
          ) {
            const fixed = previous
            const attemptId = payload.attempt_id
            if (
              fixed.body.replay.attempts.some(
                (attempt) => attempt.attempt_id === attemptId
              )
            )
              setPreviews((current) => ({
                ...current,
                [attemptId]: applyRoundtablePreview(
                  current[attemptId] ?? initialRoundtableView,
                  payload,
                  fixed,
                  subscriptionId
                ),
              }))
          } else void sync()
        }
      )
      if (!live) {
        unsubscribe()
        return
      }
      await attach()
      if (live) setStreamError(null)
    })().catch((error) => {
      if (live) setStreamError(roundtableError(error))
    })
    // Reading verified history must not depend on the live-update channel.
    void sync()
    // Private notification delivery is best effort; immutable reads recover loss.
    const timer = window.setInterval(() => {
      void sync()
    }, 5000)
    return () => {
      live = false
      window.clearInterval(timer)
      unsubscribe?.()
      reconnect?.()
      void roundtableCall("roundtable_detach", {
        room_id: roomId,
        subscription_id: subscriptionId,
      }).catch(() => undefined)
    }
  }, [roomId, refresh])

  const providerRefAt = (ordinal: number) =>
    `provider:${providerIds[ordinal] || agents[ordinal] || "codex"}`
  const formConfig = (): RoundtableConfig => {
    const original = editingDraft ? loaded?.projection.body.replay.config : null
    const waves = Math.ceil(roles.length / concurrency)
    // Proposal and critique attempts keep a per-attempt limit. Synthesis has
    // none (the server runs it to the room budget), so it only adds a room
    // allowance here and never sizes phase_budget.
    const phaseBudget = 2 * waves * MEMBER_ATTEMPT_TIMEOUT_MS
    return {
      schema_version: 1,
      ...(original?.display_name
        ? { display_name: original.display_name }
        : {}),
      topic,
      workspace_id: workspaceId,
      source_refs: original?.source_refs ?? [],
      participants: roles.map((role, ordinal) => ({
        ordinal,
        role: role.trim() || `${t("member")} ${ordinal + 1}`,
        provider_ref: providerRefAt(ordinal),
        agent: agents[ordinal] || "codex",
        ...(original?.participants.find((member) => member.ordinal === ordinal)
          ?.model &&
        original.participants.find((member) => member.ordinal === ordinal)
          ?.provider_ref === providerRefAt(ordinal)
          ? {
              model: original.participants.find(
                (member) => member.ordinal === ordinal
              )?.model,
            }
          : {}),
        ...(original?.participants.find((member) => member.ordinal === ordinal)
          ?.effort &&
        original.participants.find((member) => member.ordinal === ordinal)
          ?.provider_ref === providerRefAt(ordinal)
          ? {
              effort: original.participants.find(
                (member) => member.ordinal === ordinal
              )?.effort,
            }
          : {}),
      })),
      moderator_ordinal: moderator,
      strategy: { type: "phased_rounds", version: 1, critique_rounds: rounds },
      concurrency,
      strict_snapshot_v1: true,
      budgets: {
        room_budget: String(
          Math.max(
            Number(original?.budgets.room_budget ?? 0),
            (rounds + 1) * phaseBudget + SYNTHESIS_ROOM_ALLOWANCE_MS
          )
        ),
        phase_budget: String(
          Math.max(Number(original?.budgets.phase_budget ?? 0), phaseBudget)
        ),
      },
      timeouts: original?.timeouts ?? {
        attempt_timeout: String(MEMBER_ATTEMPT_TIMEOUT_MS),
      },
      quotas: original?.quotas ?? {
        output_byte_limit: 8192,
        input_byte_limit: 16384,
        interjection_byte_limit: 16384,
      },
    }
  }
  const config = editingDraft
    ? formConfig()
    : (loaded?.projection.body.replay.config ?? formConfig())
  const selectedPaths = roomId
    ? []
    : selectedSourcePaths
        .split(/\r?\n/)
        .map((path) => path.trim())
        .filter(Boolean)
  const sourceSelectionValid =
    selectedPaths.length <= 32 &&
    selectedPaths.every(
      (path) =>
        path.length <= 4096 &&
        !/^(?:[\\/]|[a-zA-Z]:)/.test(path) &&
        !path.split(/[\\/]/).includes("..") &&
        !path.includes("\0")
    ) &&
    new Set(
      selectedPaths.map((path) =>
        path
          .replaceAll("\\", "/")
          .split("/")
          .filter((part) => part && part !== ".")
          .join("/")
      )
    ).size === selectedPaths.length
  const configKey = JSON.stringify([
    roomId,
    loaded?.projection.body.revision,
    config,
    selectedPaths,
  ])
  const invalidate = () => {
    preflightGeneration.current += 1
    setPreflight(null)
    setConfirmed(false)
    setRecoveryConsent(false)
  }
  const check = () =>
    run(async () => {
      invalidate()
      const generation = preflightGeneration.current
      setChecking(true)
      const result = await roundtableCall<RoundtablePreflight>(
        "roundtable_preflight",
        {
          config,
          ...(roomId
            ? { room_id: roomId, revision: loaded?.projection.body.revision }
            : {}),
        }
      ).finally(() => setChecking(false))
      if (generation !== preflightGeneration.current) return
      if (result.config_hash !== (await roundtableHash(config)))
        throw new Error("preflight_config")
      setPreflight(result)
      setPreflightKey(configKey)
      setConfirmed(false)
      setRecoveryConsent(false)
    })
  const mutate = (
    command: RoundtableCommandName,
    extra: Record<string, unknown> = {}
  ) =>
    run(async () => {
      const cancellation =
        command === "roundtable_pause" || command === "roundtable_stop"
      // Cancellation has its own retry body; the unresolved paid body stays in
      // the room registry until its own acknowledgment/rejection is recovered.
      const previous =
        cancellation && mutation.current?.command === command
          ? mutation.current
          : (pendingPaidMutation(mutationScope) ?? mutation.current)
      mutation.current = prepareRoundtableMutation(
        previous,
        command,
        roomId,
        loaded?.projection.body.revision,
        extra,
        () => crypto.randomUUID()
      )
      const sent = mutation.current
      rememberPendingPaidMutation(mutationScope, sent)
      const ack = await roundtableCall<{
        room_id: string
        operation_id: string | null
      }>(command, sent.request).catch((error: unknown) => {
        const current =
          mutation.current?.request.request_id === sent.request.request_id
        if (isDefinitiveRoundtableRejection(error)) {
          clearPendingPaidMutation(mutationScope, sent.request.request_id)
          if (current) {
            mutation.current = pendingPaidMutation(mutationScope)
            setUncertainPaid(!!mutation.current)
            invalidate()
          }
        } else if (current) {
          sent.uncertain = true
          if (sent.paid) setUncertainPaid(true)
        }
        throw error
      })
      clearPendingPaidMutation(mutationScope, sent.request.request_id)
      if (mutation.current?.request.request_id !== sent.request.request_id)
        return
      mutation.current = pendingPaidMutation(mutationScope)
      setUncertainPaid(!!mutation.current)
      invalidate()
      if (command === "roundtable_update_draft") setEditingDraft(false)
      if (command === "roundtable_interject") setInterjection("")
      if (ack.operation_id)
        setOperation(
          await roundtableCall("roundtable_operation", {
            room_id: ack.room_id,
            operation_id: ack.operation_id,
          })
        )
      if (!roomId || command === "roundtable_clone")
        window.location.assign(
          `/roundtable?workspace_id=${encodeURIComponent(workspaceId)}&room_id=${encodeURIComponent(ack.room_id)}`
        )
      else setRefresh((value) => value + 1)
    })
  const canRun =
    !uncertainPaid &&
    (!roomId || (!!loaded && !roomError && !roomLoading)) &&
    preflight?.enabled === true &&
    preflight.readiness === "ready" &&
    !preflight.error &&
    sourceSelectionValid &&
    (!roomId || !!preflight.confirmed_preflight_id) &&
    preflight.capability?.recipients?.length === config.participants.length &&
    config.participants.every((participant) =>
      preflight.capability?.recipients.some(
        (recipient) =>
          recipient.ordinal === participant.ordinal &&
          recipient.provider_ref === participant.provider_ref &&
          !!recipient.origin &&
          !!recipient.model &&
          !!recipient.agent
      )
    ) &&
    confirmed &&
    preflightKey === configKey &&
    !editingDraft
  const projection = loaded?.projection
  const status = projection?.body.status
  const liveInputs = useMemo(
    () => liveTranscriptInputs(liveOutput, projection),
    [liveOutput, projection]
  )
  // Keep the room list's status in step with the verified room snapshot.
  useEffect(() => {
    if (!roomId || !status) return
    setRooms((current) =>
      current.some((room) => room.room_id === roomId && room.status !== status)
        ? current.map((room) =>
            room.room_id === roomId ? { ...room, status } : room
          )
        : current
    )
  }, [roomId, status])
  const buttonClass =
    "h-auto min-h-9 max-w-full justify-start whitespace-normal py-2 text-start"
  const editDraft = () => {
    const members = [...config.participants].sort(
      (left, right) => left.ordinal - right.ordinal
    )
    setTopic(config.topic)
    setRoles(members.map((member) => member.role))
    setProviderIds(
      members.map((member) => {
        const id = member.provider_ref.replace(/^provider:/, "")
        return id === (member.agent || "codex") ? "" : id
      })
    )
    agentsTouched.current = true
    setAgents(members.map((member) => member.agent || "codex"))
    setRounds(config.strategy.critique_rounds)
    setConcurrency(config.concurrency)
    setModerator(config.moderator_ordinal)
    setEditingDraft(true)
    invalidate()
  }

  const members: ComposerMember[] = roles.map((role, index) => ({
    role,
    agent: agents[index] || "codex",
    providerId: providerIds[index] || "",
  }))
  const updateMember = (index: number, patch: Partial<ComposerMember>) => {
    if (patch.role !== undefined) {
      const role = patch.role
      setRoles(roles.map((old, i) => (i === index ? role : old)))
    }
    if (patch.agent !== undefined) {
      const agent = patch.agent
      agentsTouched.current = true
      setAgents(
        members.map((member, i) => (i === index ? agent : member.agent))
      )
    }
    if (patch.providerId !== undefined) {
      const providerId = patch.providerId
      setProviderIds(
        members.map((member, i) =>
          i === index ? providerId : member.providerId
        )
      )
    }
    invalidate()
  }
  const addMember = () => {
    if (roles.length >= ROUNDTABLE_MAX_MEMBERS) return
    const next = nextRoundtableAgent(
      members.map((member) => member.agent),
      agentStatus
    )
    setRoles([...roles, ""])
    setAgents([...members.map((member) => member.agent), next])
    setProviderIds([...members.map((member) => member.providerId), ""])
    invalidate()
  }
  const removeMember = (index: number) => {
    if (roles.length <= ROUNDTABLE_MIN_MEMBERS) return
    const keep = (_: unknown, i: number) => i !== index
    setRoles(roles.filter(keep))
    setAgents(members.map((member) => member.agent).filter(keep))
    setProviderIds(members.map((member) => member.providerId).filter(keep))
    setConcurrency(Math.min(concurrency, roles.length - 1))
    setModerator(
      moderator === index ? 0 : moderator > index ? moderator - 1 : moderator
    )
    invalidate()
  }
  // Drafts created without a pinned model store "default" until the run
  // resolves the seat's binding; show the checked model or the agent instead.
  const speakerModelLabel = (
    modelId: string,
    seatOrdinal: number | null,
    agent: string
  ) => {
    if (modelId && modelId !== "default") return modelId
    const resolved =
      preflight && preflightKey === configKey && seatOrdinal !== null
        ? preflight.capability?.recipients.find(
            (recipient) => recipient.ordinal === seatOrdinal
          )?.model
        : undefined
    return resolved || roundtableAgentLabel(agent)
  }
  const budgetMinutes = Math.ceil(Number(config.budgets.room_budget) / 60000)
  const preflightCurrent = !!preflight && preflightKey === configKey
  const preflightOk =
    preflightCurrent &&
    preflight.enabled &&
    preflight.readiness === "ready" &&
    !preflight.error
  const currentWorkspace = workspaces?.find(
    (folder) => String(folder.id) === workspaceId
  )
  const workspaceUnavailable =
    !!workspaceId && !!workspaces && !currentWorkspace
  const selectWorkspace = (next: string) => {
    if (next === workspaceId) return
    setWorkspaceId(next)
    invalidate()
    setError(null)
    // Keep the URL (and the room list links built from it) on the new workspace
    // without remounting the form.
    window.history.replaceState(
      null,
      "",
      `/roundtable?workspace_id=${encodeURIComponent(next)}`
    )
  }
  const preflightDisabled =
    busy ||
    !workspaceId ||
    (!roomId && workspaceUnavailable) ||
    (!!roomId && (!loaded || !!roomError || roomLoading)) ||
    !sourceSelectionValid ||
    editingDraft ||
    (!roomId && !topic.trim())
  const preflightPanel = (
    <div className="flex min-w-0 flex-col gap-3">
      <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-2">
        <Button
          variant="outline"
          disabled={preflightDisabled}
          onClick={() => void check()}
        >
          {checking ? (
            <Loader2 aria-hidden="true" className="animate-spin" />
          ) : (
            <ShieldCheck aria-hidden="true" />
          )}
          {t("preflight")}
        </Button>
        {checking ? (
          <p role="status" className="text-sm text-muted-foreground">
            {t("checking")}
          </p>
        ) : !roomId && !topic.trim() ? (
          <p className="text-sm text-muted-foreground">{t("topicRequired")}</p>
        ) : null}
      </div>
      {preflight && preflightCurrent ? (
        <>
          <div
            role="status"
            className={cn(
              "flex min-w-0 items-start gap-2 rounded-lg border p-3 text-sm",
              preflightOk
                ? "border-emerald-600/30 bg-emerald-500/5 text-emerald-800 dark:border-emerald-400/30 dark:text-emerald-300"
                : "border-amber-600/30 bg-amber-500/5 text-amber-900 dark:border-amber-400/30 dark:text-amber-200"
            )}
          >
            {preflightOk ? (
              <CircleCheck
                aria-hidden="true"
                className="mt-0.5 size-4 shrink-0"
              />
            ) : (
              <TriangleAlert
                aria-hidden="true"
                className="mt-0.5 size-4 shrink-0"
              />
            )}
            <span>{preflight.enabled ? t("enabled") : t("disabled")}</span>
          </div>
          {preflight.error ? (
            <p
              role="alert"
              className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive [overflow-wrap:anywhere]"
            >
              {describeError(preflight.error)}
            </p>
          ) : null}
          <PreflightConfirmation
            recipients={preflight.capability?.recipients ?? []}
            moderatorOrdinal={config.moderator_ordinal}
            seatBrands={roundtableSeatBrands(config)}
            sourceManifests={preflight.source_manifests ?? []}
            selectedPaths={selectedPaths}
            sourcePreviews={sourcePreviews}
            onPreview={
              roomId
                ? (entry) =>
                    void run(async () => {
                      const text = await loadRoundtableSource(roomId, entry)
                      setSourcePreviews((previous) => ({
                        ...previous,
                        [entry.content_hash]: text,
                      }))
                    })
                : undefined
            }
            tools={preflight.tools}
            network={preflight.network}
            writes={preflight.writes}
            workspaceMount={preflight.workspace_mount}
            budget={`${Math.ceil(Number(config.budgets.room_budget) / 60000)} ${t("minutes")}`}
            attemptLimit={`${Math.ceil(Number(config.timeouts.attempt_timeout) / 60000)} ${t("minutes")}`}
          />
          <label className="flex min-w-0 cursor-pointer items-start gap-3 rounded-lg border bg-background p-3 text-sm leading-relaxed transition-colors has-checked:border-primary/60 has-checked:bg-primary/5 has-[:focus-visible]:ring-[3px] has-[:focus-visible]:ring-ring/50">
            <input
              type="checkbox"
              className="mt-0.5 size-4 shrink-0 accent-primary"
              checked={confirmed}
              onChange={(event) => setConfirmed(event.target.checked)}
            />
            <span>{t("confirm")}</span>
          </label>
        </>
      ) : null}
    </div>
  )

  return (
    <main className="mx-auto flex w-full min-w-0 max-w-6xl flex-col gap-5 overflow-auto p-4 sm:p-6">
      <header className="flex flex-wrap items-center justify-between gap-3 border-b pb-4">
        <div className="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-1">
          <h1 className="text-2xl font-semibold tracking-tight">
            {t("title")}
          </h1>
          {currentWorkspace ? (
            <span
              className="flex min-w-0 items-center gap-1.5 text-sm text-muted-foreground"
              title={currentWorkspace.path}
            >
              <FolderOpen aria-hidden="true" className="size-3.5 shrink-0" />
              <span className="truncate">{currentWorkspace.name}</span>
            </span>
          ) : null}
        </div>
        <Link
          href="/workspace"
          className="rounded-md text-sm text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
        >
          {t("back")}
        </Link>
      </header>
      {error ? (
        <p
          role="alert"
          className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive [overflow-wrap:anywhere]"
        >
          {error}
        </p>
      ) : null}
      {!workspaceId && roomId ? (
        <p role="alert">{t("workspaceRequired")}</p>
      ) : null}
      {uncertainPaid ? (
        <section aria-label={t("pendingPaidOperation")}>
          <p role="status">{t("pendingPaidOperation")}</p>
          <p>{t("pendingReloadBoundary")}</p>
          <Button
            disabled={busy}
            onClick={() => {
              const pending = pendingPaidMutation(mutationScope)
              if (pending) void mutate(pending.command, pending.extra)
            }}
          >
            {t("retryPendingOperation")}
          </Button>
        </section>
      ) : null}
      <div className="grid min-w-0 gap-5 md:grid-cols-[15rem_minmax(0,1fr)]">
        <RoundtableRoomList
          workspaceId={workspaceId}
          roomId={roomId}
          rooms={rooms}
          loading={listLoading}
          error={listError}
          cursor={listCursor}
          onRefresh={() => void listRooms()}
          onMore={() => {
            if (listCursor) void listRooms(listCursor)
          }}
        />
        <section className="flex min-w-0 flex-col gap-5 rounded-xl border bg-card p-4 [overflow-wrap:anywhere] sm:p-5 [&_button]:h-auto [&_button]:min-h-9 [&_button]:max-w-full [&_button]:whitespace-normal [&_button]:py-2 [&_select]:max-w-full">
          {providersError && (!roomId || editingDraft) ? (
            <div role="alert" className="space-y-2 text-sm text-destructive">
              <p>{providersError}</p>
              <Button
                variant="outline"
                disabled={providersLoading}
                onClick={() => void loadProviders()}
              >
                {t("retryProviders")}
              </Button>
            </div>
          ) : null}
          {roomError ? (
            <div
              role="alert"
              className="space-y-2 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive"
            >
              {loaded ? <p>{t("refreshFailed")}</p> : null}
              <p>{roomError}</p>
              <Button
                variant="outline"
                disabled={roomLoading}
                onClick={() => setRefresh((value) => value + 1)}
              >
                {t("retryLoad")}
              </Button>
            </div>
          ) : null}
          {streamError ? (
            <div className="space-y-2 rounded-lg border p-3 text-sm text-muted-foreground">
              <p role="status">{t("liveUpdatesUnavailable")}</p>
              {!roomError ? (
                <Button
                  variant="outline"
                  disabled={roomLoading}
                  onClick={() => setRefresh((value) => value + 1)}
                >
                  {t("retryLoad")}
                </Button>
              ) : null}
            </div>
          ) : null}
          {!roomId || editingDraft ? (
            <>
              <div className="min-w-0 space-y-1">
                <h2 className="text-xl font-semibold tracking-tight">
                  {editingDraft ? t("editDraft") : t("new")}
                </h2>
                <p className="text-sm leading-relaxed text-muted-foreground">
                  {t("newSubtitle")}
                </p>
              </div>
              <RoundtableComposer
                workspaceField={
                  !roomId ? (
                    <RoundtableWorkspaceSelect
                      workspaces={workspaces}
                      loading={workspacesLoading}
                      error={workspacesError}
                      value={workspaceId}
                      disabled={busy || uncertainPaid}
                      onChange={selectWorkspace}
                      onRetry={() => void loadWorkspaces()}
                    />
                  ) : undefined
                }
                sourceRoot={currentWorkspace?.path}
                workspaceId={workspaceId}
                topic={topic}
                onTopicChange={(value) => {
                  setTopic(value)
                  invalidate()
                }}
                showSources={!roomId}
                sourcePaths={selectedSourcePaths}
                onSourcePathsChange={(value) => {
                  setSelectedSourcePaths(value)
                  invalidate()
                }}
                sourceCount={selectedPaths.length}
                sourceValid={sourceSelectionValid}
                members={members}
                agentStatus={agentStatus}
                agentStatusLoading={agentsLoading}
                onRecheckAgents={() => {
                  invalidate()
                  void loadAgents()
                }}
                seatBrands={roundtableSeatBrands(config)}
                providers={providers}
                onMemberChange={updateMember}
                onAddMember={addMember}
                onRemoveMember={removeMember}
                moderator={moderator}
                onModeratorChange={(index) => {
                  setModerator(index)
                  invalidate()
                }}
                rounds={rounds}
                onRoundsChange={(value) => {
                  setRounds(value)
                  invalidate()
                }}
                concurrency={concurrency}
                onConcurrencyChange={(value) => {
                  setConcurrency(value)
                  invalidate()
                }}
                budgetMinutes={budgetMinutes}
              />
            </>
          ) : projection ? (
            <>
              <h2 className="text-xl leading-relaxed font-semibold tracking-tight [overflow-wrap:anywhere]">
                <RoundtableInlineText text={config.topic} />
              </h2>
              <p className="text-sm text-muted-foreground">
                {t("status")}: {status}
              </p>
              {projection.body.blocked_reason ? (
                <p role="status">{projection.body.blocked_reason}</p>
              ) : null}
              <ol className="flex flex-wrap gap-2 rounded-lg bg-muted/40 p-3 text-sm leading-relaxed">
                {projection.body.replay.speakers.map((speaker) => {
                  const seatBrands = roundtableSeatBrands(
                    projection.body.replay.config
                  )
                  const replayConfig = projection.body.replay.config
                  const seatOrdinal =
                    speaker.role === "moderator"
                      ? (replayConfig?.moderator_ordinal ?? null)
                      : speaker.ordinal
                  const agent =
                    replayConfig?.participants.find(
                      (participant) => participant.ordinal === seatOrdinal
                    )?.agent ?? "codex"
                  const seat = speakerSeatBrand(
                    {
                      seatOrdinal,
                      agent,
                      modelId: speaker.model_id,
                      providerRef: speaker.provider_ref,
                    },
                    seatBrands
                  )
                  return (
                    <li
                      key={speaker.speaker_id}
                      data-brand={seat.key}
                      style={brandStyle(seat.brand)}
                      className={cn(
                        "flex min-h-8 min-w-0 items-center gap-2 rounded-2xl border bg-background py-1 ps-1 pe-3",
                        BRAND_CLASSES.border
                      )}
                    >
                      <RoundtableSpeakerAvatar
                        agent={agent}
                        seat={seat}
                        seatNumber={
                          seatOrdinal !== null ? seatOrdinal + 1 : null
                        }
                        size="sm"
                      />
                      <span className="min-w-0 leading-5 [overflow-wrap:anywhere]">
                        {speaker.role === "moderator"
                          ? t("moderator")
                          : `${t("member")} ${speaker.ordinal + 1}`}{" "}
                        ·{" "}
                        {speakerModelLabel(
                          speaker.model_id,
                          seatOrdinal,
                          agent
                        )}
                      </span>
                      <span className="text-xs whitespace-nowrap text-muted-foreground">
                        {projection.body.replay.attempts
                          .filter((attempt) =>
                            projection.body.replay.turns.some(
                              (turn) =>
                                turn.turn_id === attempt.turn_id &&
                                turn.speaker_id === speaker.speaker_id
                            )
                          )
                          .at(-1)?.state ?? t("waiting")}
                      </span>
                    </li>
                  )
                })}
              </ol>
              <RoundtableTranscript
                projection={projection}
                messages={loaded?.messages ?? []}
                previews={previews}
                live={liveInputs}
                conclusionActions={
                  status === "completed" && roomId ? (
                    <RoundtableConclusionActions roomId={roomId} />
                  ) : undefined
                }
              />
              <section
                aria-label={t("evidence")}
                className="flex flex-col gap-2"
              >
                {projection.body.replay.evidence
                  .filter((item) => item.published_seq !== null)
                  .map((item) => (
                    <div
                      key={item.evidence_id}
                      className="rounded-lg border p-3"
                    >
                      <Button
                        variant="outline"
                        disabled={busy}
                        onClick={() =>
                          void run(async () => {
                            const verified = await loadRoundtableEvidence(
                              projection,
                              item.evidence_id
                            )
                            setEvidence((previous) => ({
                              ...previous,
                              [item.evidence_id]: verified,
                            }))
                          })
                        }
                      >
                        {t("evidence")} · {item.evidence_id.slice(0, 8)}
                      </Button>
                      {evidence[item.evidence_id] ? (
                        <>
                          <p>
                            {evidence[item.evidence_id].body.file_alias} ·{" "}
                            {evidence[item.evidence_id].body.verified
                              ? t("verified")
                              : t("unverified")}
                          </p>
                          <RoundtableSafeContent
                            text={evidence[item.evidence_id].body.excerpt}
                            preview={false}
                          />
                        </>
                      ) : null}
                    </div>
                  ))}
              </section>
            </>
          ) : !roomError ? (
            <p role="status">{t("loading")}</p>
          ) : null}
          {!roomId ? (
            <RoundtableStep
              step={5}
              title={t("review")}
              description={t("reviewHelp")}
            >
              {preflightPanel}
              <div className="mt-4 flex flex-wrap items-center gap-3 border-t pt-4">
                <Button
                  size="lg"
                  className="w-full sm:w-auto sm:min-w-40"
                  disabled={busy || !canRun}
                  onClick={() =>
                    void mutate("roundtable_create", {
                      config,
                      selected_source_paths: selectedPaths,
                    })
                  }
                >
                  {busy && !checking ? (
                    <Loader2 aria-hidden="true" className="animate-spin" />
                  ) : null}
                  {t("create")}
                </Button>
                {!canRun && preflightOk && !confirmed ? (
                  <p className="text-sm text-muted-foreground">
                    {t("confirmToCreate")}
                  </p>
                ) : null}
              </div>
            </RoundtableStep>
          ) : (
            <div className="min-w-0 rounded-xl border bg-muted/20 p-3 sm:p-4">
              {preflightPanel}
            </div>
          )}
          <div className="flex flex-wrap gap-2">
            {!roomId ? null : (
              <>
                {status === "draft" || status === "ready" ? (
                  <>
                    {editingDraft ? (
                      <>
                        <Button
                          disabled={busy || !topic.trim()}
                          onClick={() =>
                            void mutate("roundtable_update_draft", { config })
                          }
                        >
                          {t("saveDraft")}
                        </Button>
                        <Button
                          variant="outline"
                          disabled={busy}
                          onClick={() => {
                            setEditingDraft(false)
                            invalidate()
                          }}
                        >
                          {t("cancel")}
                        </Button>
                      </>
                    ) : (
                      <Button
                        variant="outline"
                        disabled={busy}
                        onClick={editDraft}
                      >
                        {t("editDraft")}
                      </Button>
                    )}
                    <Button
                      disabled={busy || !canRun}
                      onClick={() =>
                        void mutate("roundtable_start", {
                          confirmed_preflight_id:
                            preflight?.confirmed_preflight_id ?? undefined,
                        })
                      }
                    >
                      {t("start")}
                    </Button>
                  </>
                ) : null}
                {status === "running" ? (
                  <Button
                    className={buttonClass}
                    disabled={busy}
                    onClick={() =>
                      void mutate("roundtable_pause", { reason: "operator" })
                    }
                  >
                    {t("pause")}
                  </Button>
                ) : null}
                {status === "paused" ? (
                  <>
                    <label>
                      <input
                        type="checkbox"
                        checked={recoveryConsent}
                        onChange={(event) =>
                          setRecoveryConsent(event.target.checked)
                        }
                      />{" "}
                      {t("recoveryConsent")}
                    </label>
                    <Button
                      disabled={busy || !canRun || !recoveryConsent}
                      onClick={() =>
                        void mutate("roundtable_resume", {
                          recovery_consent: recoveryConsent,
                          confirmed_preflight_id:
                            preflight?.confirmed_preflight_id ?? undefined,
                        })
                      }
                    >
                      {t("resume")}
                    </Button>
                  </>
                ) : null}
                {projection?.body.blocked_reason === "synthesis_failed" ? (
                  <Button
                    disabled={busy || !canRun}
                    onClick={() =>
                      void mutate("roundtable_retry_synthesis", {
                        confirmed_preflight_id:
                          preflight?.confirmed_preflight_id ?? undefined,
                      })
                    }
                  >
                    {t("retry")}
                  </Button>
                ) : null}
                {status &&
                !["completed", "stopped", "failed", "draft"].includes(
                  status
                ) ? (
                  <Button
                    variant="destructive"
                    disabled={busy}
                    onClick={() => void mutate("roundtable_stop")}
                  >
                    {t("stop")}
                  </Button>
                ) : null}
                <Button
                  variant="outline"
                  disabled={busy || !canRun}
                  onClick={() =>
                    void mutate("roundtable_clone", {
                      carry_published_context: false,
                    })
                  }
                >
                  {t("clone")}
                </Button>
                <Button
                  variant="outline"
                  disabled={busy || !loaded || !!roomError || roomLoading}
                  onClick={() =>
                    void run(async () => {
                      setUsage(
                        await roundtableCall("roundtable_get", {
                          room_id: roomId,
                          read: { usage: {} },
                        })
                      )
                    })
                  }
                >
                  {t("usage")}
                </Button>
              </>
            )}
          </div>
          {roomId && status && ["running", "paused"].includes(status) ? (
            <section className="flex flex-col gap-2">
              <label>
                {t("interjection")}
                <Textarea
                  value={interjection}
                  onChange={(event) => setInterjection(event.target.value)}
                />
              </label>
              <select
                aria-label={t("interjectionMode")}
                className="rounded border bg-background p-2"
                value={interjectMode}
                onChange={(event) => setInterjectMode(event.target.value)}
              >
                <option value="next_phase">{t("nextPhase")}</option>
                <option value="restart_current">{t("restart")}</option>
              </select>
              <Button
                disabled={
                  busy ||
                  uncertainPaid ||
                  !interjection.trim() ||
                  (interjectMode === "restart_current" && !canRun)
                }
                onClick={() =>
                  void mutate("roundtable_interject", {
                    text: interjection,
                    mode: interjectMode,
                    ...(interjectMode === "restart_current"
                      ? {
                          confirmed_preflight_id:
                            preflight?.confirmed_preflight_id ?? undefined,
                        }
                      : {}),
                  })
                }
              >
                {t("send")}
              </Button>
            </section>
          ) : null}
          {operation ? (
            <section
              className="rounded-lg border p-3"
              aria-label={t("operation")}
            >
              <p>
                {t("operation")}: {operation.kind}
              </p>
              <p>
                {t("status")}: {operation.status} · {operation.step}
              </p>
              {operation.blocked_reason ? (
                <p role="status">{operation.blocked_reason}</p>
              ) : null}
              <Button
                variant="outline"
                disabled={busy}
                onClick={() =>
                  void run(async () =>
                    setOperation(
                      await roundtableCall("roundtable_operation", {
                        room_id: roomId,
                        operation_id: operation.operation_id,
                      })
                    )
                  )
                }
              >
                {t("refresh")}
              </Button>
            </section>
          ) : null}
          {usage ? (
            <dl className="rounded-lg border p-3">
              {usage.totals.map((total) => (
                <div key={total.key}>
                  <dt>
                    {total.key === "active_ms"
                      ? t("activeTime")
                      : total.key === "attempts"
                        ? t("attempts")
                        : total.key}
                  </dt>
                  <dd>
                    {total.value}
                    {total.key === "active_ms" ? " ms" : ""}
                  </dd>
                </div>
              ))}
              <dt>{t("outputTokens")}</dt>
              <dd>{usage.confirmed_output_tokens ?? t("unknown")}</dd>
              <dt>{t("unknownUsage")}</dt>
              <dd>{usage.unknown_count}</dd>
            </dl>
          ) : null}
        </section>
      </div>
    </main>
  )
}
