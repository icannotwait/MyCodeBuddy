"use client"

import Link from "next/link"
import { useCallback, useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { listModelProviders } from "@/lib/api"
import { getTransport } from "@/lib/transport"
import { initialRoundtableView } from "@/lib/roundtable/reducer"
import { applyRoundtablePreview } from "@/lib/roundtable/stream"
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
  prepareRoundtableMutation,
  isDefinitiveRoundtableRejection,
  pendingPaidMutation,
  rememberPendingPaidMutation,
  clearPendingPaidMutation,
} from "@/lib/roundtable/mutation"
import type { PendingRoundtableMutation } from "@/lib/roundtable/mutation"
import type { ModelProviderInfo } from "@/lib/types"
import type {
  RoundtableConfig,
  RoundtablePreflight,
  RoundtableProjection,
  RoundtableEvidence,
  RoundtableOperation,
  RoundtableUsage,
  RoundtableView,
} from "@/lib/roundtable/types"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { PreflightConfirmation } from "./preflight-confirmation"
import { RoundtableSafeContent } from "./roundtable-safe-content"
import { RoundtableRoomList } from "./roundtable-room-list"
import { RoundtableTranscript } from "./roundtable-transcript"

type LoadedRoom = Awaited<ReturnType<typeof loadRoundtable>>
type RoomSummary = { room_id: string; status: string; config: RoundtableConfig }

const ROUNDTABLE_AGENTS = ["grok", "cursor", "antigravity", "codex"] as const

export function RoundtableWorkbench({
  workspaceId,
  roomId,
}: {
  workspaceId: string
  roomId?: string
}) {
  const t = useTranslations("Roundtable")
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
  const [roles, setRoles] = useState(["", "", ""])
  const [agents, setAgents] = useState<string[]>([
    "grok",
    "cursor",
    "antigravity",
  ])
  const [providerIds, setProviderIds] = useState<string[]>([])
  const [rounds, setRounds] = useState(2)
  const [concurrency, setConcurrency] = useState(3)
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
  const actionInFlight = useRef(false)
  const [error, setError] = useState<string | null>(null)
  const [interjection, setInterjection] = useState("")
  const [interjectMode, setInterjectMode] = useState("next_phase")
  const [refresh, setRefresh] = useState(0)
  const [operation, setOperation] = useState<RoundtableOperation | null>(null)
  const [usage, setUsage] = useState<RoundtableUsage | null>(null)
  const [previews, setPreviews] = useState<Record<string, RoundtableView>>({})
  const mutationScope = JSON.stringify([workspaceId, roomId])
  const mutation = useRef<PendingRoundtableMutation | null>(
    pendingPaidMutation(mutationScope)
  )
  const [uncertainPaid, setUncertainPaid] = useState(
    () => !!pendingPaidMutation(mutationScope)
  )

  const run = useCallback(async (action: () => Promise<void>) => {
    if (actionInFlight.current) return
    actionInFlight.current = true
    setBusy(true)
    setError(null)
    try {
      await action()
    } catch (error) {
      setError(roundtableError(error))
    } finally {
      actionInFlight.current = false
      setBusy(false)
    }
  }, [])

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

  useEffect(() => {
    void loadProviders()
    return () => {
      providersGeneration.current += 1
      providersInFlight.current = false
    }
  }, [loadProviders])

  useEffect(() => {
    if (!roomId) return
    let live = true
    let syncing = false
    let previous: RoundtableProjection | null = null
    let unsubscribe: (() => void) | undefined
    setRoomLoading(true)
    setRoomError(null)
    setStreamError(null)
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

  const formConfig = (): RoundtableConfig => {
    const original = editingDraft ? loaded?.projection.body.replay.config : null
    const waves = Math.ceil(roles.length / concurrency)
    const phaseBudget = 2 * waves * 225000
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
        provider_ref: `provider:${providerIds[ordinal] || providers[0]?.id || ""}`,
        agent: agents[ordinal] || "codex",
        ...(original?.participants.find((member) => member.ordinal === ordinal)
          ?.model &&
        original.participants.find((member) => member.ordinal === ordinal)
          ?.provider_ref === `provider:${providerIds[ordinal]}`
          ? {
              model: original.participants.find(
                (member) => member.ordinal === ordinal
              )?.model,
            }
          : {}),
        ...(original?.participants.find((member) => member.ordinal === ordinal)
          ?.effort &&
        original.participants.find((member) => member.ordinal === ordinal)
          ?.provider_ref === `provider:${providerIds[ordinal]}`
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
            (rounds + 1) * phaseBudget + 450000
          )
        ),
        phase_budget: String(
          Math.max(Number(original?.budgets.phase_budget ?? 0), phaseBudget)
        ),
      },
      timeouts: original?.timeouts ?? { attempt_timeout: "225000" },
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
      const result = await roundtableCall<RoundtablePreflight>(
        "roundtable_preflight",
        {
          config,
          ...(roomId
            ? { room_id: roomId, revision: loaded?.projection.body.revision }
            : {}),
        }
      )
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
  const buttonClass =
    "h-auto min-h-9 max-w-full justify-start whitespace-normal py-2 text-start"
  const editDraft = () => {
    const members = [...config.participants].sort(
      (left, right) => left.ordinal - right.ordinal
    )
    setTopic(config.topic)
    setRoles(members.map((member) => member.role))
    setProviderIds(
      members.map((member) => member.provider_ref.replace(/^provider:/, ""))
    )
    setAgents(members.map((member) => member.agent || "codex"))
    setRounds(config.strategy.critique_rounds)
    setConcurrency(config.concurrency)
    setModerator(config.moderator_ordinal)
    setEditingDraft(true)
    invalidate()
  }

  return (
    <main className="mx-auto flex w-full min-w-0 max-w-6xl flex-col gap-5 overflow-auto p-4 sm:p-6">
      <header className="flex flex-wrap items-center justify-between gap-3 border-b pb-4">
        <h1 className="text-2xl font-semibold tracking-tight">{t("title")}</h1>
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
      {!workspaceId ? <p role="alert">{t("workspaceRequired")}</p> : null}
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
              <label>
                {t("topic")}
                <Textarea
                  aria-label={t("topic")}
                  value={topic}
                  onChange={(event) => {
                    setTopic(event.target.value)
                    invalidate()
                  }}
                />
              </label>
              {!roomId ? (
                <label>
                  {t("selectedSourcePaths")}
                  <Textarea
                    aria-label={t("selectedSourcePaths")}
                    value={selectedSourcePaths}
                    placeholder={t("sourcePathsPlaceholder")}
                    onChange={(event) => {
                      setSelectedSourcePaths(event.target.value)
                      invalidate()
                    }}
                  />
                  <p className="text-sm text-muted-foreground">
                    {t("sourceSelectionHelp")}
                  </p>
                  <p className="text-sm text-muted-foreground">
                    {t("sourceRetentionNotice")}
                  </p>
                  {!sourceSelectionValid ? (
                    <p role="alert">{t("invalidSourceSelection")}</p>
                  ) : null}
                </label>
              ) : null}
              {roles.map((role, index) => (
                <fieldset
                  key={index}
                  className="grid min-w-0 gap-3 rounded-lg border p-3"
                >
                  <legend>
                    {t("member")} {index + 1}
                  </legend>
                  <Input
                    aria-label={`${t("role")} ${index + 1}`}
                    value={role}
                    onChange={(event) => {
                      setRoles(
                        roles.map((old, i) =>
                          i === index ? event.target.value : old
                        )
                      )
                      invalidate()
                    }}
                  />
                  <label>
                    {t("agent")}
                    <select
                      aria-label={`${t("agent")} ${index + 1}`}
                      className="ml-2 rounded border bg-background p-2"
                      value={agents[index] || "codex"}
                      onChange={(event) => {
                        setAgents(
                          roles.map((_, i) =>
                            i === index
                              ? event.target.value
                              : agents[i] || "codex"
                          )
                        )
                        invalidate()
                      }}
                    >
                      {ROUNDTABLE_AGENTS.map((agent) => (
                        <option key={agent} value={agent}>
                          {agent}
                        </option>
                      ))}
                    </select>
                  </label>
                  <label>
                    {t("provider")}
                    <select
                      aria-label={`${t("provider")} ${index + 1}`}
                      className="ml-2 rounded border bg-background p-2"
                      value={providerIds[index] || providers[0]?.id || ""}
                      onChange={(event) => {
                        setProviderIds(
                          roles.map((_, i) =>
                            i === index
                              ? event.target.value
                              : providerIds[i] || String(providers[0]?.id || "")
                          )
                        )
                        invalidate()
                      }}
                    >
                      {providers.map((provider) => (
                        <option key={provider.id} value={provider.id}>
                          {provider.name}
                        </option>
                      ))}
                    </select>
                  </label>
                </fieldset>
              ))}
              <div className="flex flex-wrap gap-2">
                <Button
                  variant="outline"
                  disabled={roles.length >= 7}
                  onClick={() => {
                    setRoles([...roles, ""])
                    setAgents([...agents, "codex"])
                    invalidate()
                  }}
                >
                  {t("addMember")}
                </Button>
                <Button
                  variant="outline"
                  disabled={roles.length <= 2}
                  onClick={() => {
                    setRoles(roles.slice(0, -1))
                    setAgents(agents.slice(0, -1))
                    setConcurrency(Math.min(concurrency, roles.length - 1))
                    setModerator(Math.min(moderator, roles.length - 2))
                    invalidate()
                  }}
                >
                  {t("removeMember")}
                </Button>
              </div>
              <div className="flex flex-wrap gap-4">
                <label>
                  {t("rounds")}
                  <Input
                    type="number"
                    min={0}
                    max={5}
                    value={rounds}
                    onChange={(event) => {
                      setRounds(
                        Math.max(0, Math.min(5, Number(event.target.value)))
                      )
                      invalidate()
                    }}
                  />
                </label>
                <label>
                  {t("concurrency")}
                  <Input
                    type="number"
                    min={1}
                    max={roles.length}
                    value={concurrency}
                    onChange={(event) => {
                      setConcurrency(
                        Math.max(
                          1,
                          Math.min(roles.length, Number(event.target.value))
                        )
                      )
                      invalidate()
                    }}
                  />
                </label>
                <label>
                  {t("moderator")}
                  <select
                    className="ml-2 rounded border bg-background p-2"
                    value={moderator}
                    onChange={(event) => {
                      setModerator(Number(event.target.value))
                      invalidate()
                    }}
                  >
                    {roles.map((role, index) => (
                      <option key={index} value={index}>
                        {role || `${t("member")} ${index + 1}`}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
            </>
          ) : projection ? (
            <>
              <h2 className="text-xl font-semibold leading-relaxed tracking-tight">
                {config.topic}
              </h2>
              <p className="text-sm text-muted-foreground">
                {t("status")}: {status}
              </p>
              {projection.body.blocked_reason ? (
                <p role="status">{projection.body.blocked_reason}</p>
              ) : null}
              <ol className="space-y-2 rounded-lg bg-muted/40 p-3 text-sm leading-relaxed">
                {projection.body.replay.speakers.map((speaker) => (
                  <li key={speaker.speaker_id}>
                    {speaker.role === "moderator"
                      ? t("moderator")
                      : `${t("member")} ${speaker.ordinal + 1}`}{" "}
                    · {speaker.model_id}
                    <span className="ml-2 text-muted-foreground">
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
                ))}
              </ol>
              <RoundtableTranscript
                projection={projection}
                messages={loaded?.messages ?? []}
                previews={previews}
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
          <Button
            variant="outline"
            disabled={
              busy ||
              !workspaceId ||
              (!!roomId && (!loaded || !!roomError || roomLoading)) ||
              !sourceSelectionValid ||
              editingDraft ||
              (!roomId && (!topic.trim() || providers.length === 0))
            }
            onClick={() => void check()}
          >
            {t("preflight")}
          </Button>
          {preflight && preflightKey === configKey ? (
            <>
              <p role="status">
                {preflight.enabled ? t("enabled") : t("disabled")}
              </p>
              {preflight.error ? (
                <p role="alert">{roundtableError(preflight.error)}</p>
              ) : null}
              <PreflightConfirmation
                recipients={preflight.capability?.recipients ?? []}
                moderatorOrdinal={config.moderator_ordinal}
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
                budget={`${Math.ceil(Number(config.budgets.room_budget) / 60000)} ${t("minutes")}`}
              />
              <label>
                <input
                  type="checkbox"
                  checked={confirmed}
                  onChange={(event) => setConfirmed(event.target.checked)}
                />{" "}
                {t("confirm")}
              </label>
            </>
          ) : null}
          <div className="flex flex-wrap gap-2">
            {!roomId ? (
              <Button
                disabled={busy || !canRun}
                onClick={() =>
                  void mutate("roundtable_create", {
                    config,
                    selected_source_paths: selectedPaths,
                  })
                }
              >
                {t("create")}
              </Button>
            ) : (
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
