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
  roundtableCall,
  roundtableError,
  roundtableHash,
  verifyRoundtableReplay,
} from "@/lib/roundtable/api"
import type { RoundtableCommandName } from "@/lib/roundtable/api"
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

type LoadedRoom = Awaited<ReturnType<typeof loadRoundtable>>
type RoomSummary = { room_id: string; status: string; config: RoundtableConfig }

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
  const [loaded, setLoaded] = useState<LoadedRoom | null>(null)
  const [topic, setTopic] = useState("")
  const [roles, setRoles] = useState(["", "", ""])
  const [providerIds, setProviderIds] = useState<string[]>([])
  const [rounds, setRounds] = useState(2)
  const [concurrency, setConcurrency] = useState(3)
  const [moderator, setModerator] = useState(0)
  const [preflight, setPreflight] = useState<RoundtablePreflight | null>(null)
  const [confirmed, setConfirmed] = useState(false)
  const [preflightKey, setPreflightKey] = useState<string | null>(null)
  const [recoveryConsent, setRecoveryConsent] = useState(false)
  const [editingDraft, setEditingDraft] = useState(false)
  const [evidence, setEvidence] = useState<Record<string, RoundtableEvidence>>(
    {}
  )
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [interjection, setInterjection] = useState("")
  const [interjectMode, setInterjectMode] = useState("next_phase")
  const [refresh, setRefresh] = useState(0)
  const [operation, setOperation] = useState<RoundtableOperation | null>(null)
  const [usage, setUsage] = useState<RoundtableUsage | null>(null)
  const [previews, setPreviews] = useState<Record<string, RoundtableView>>({})
  const mutation = useRef<{
    key: string
    request: Record<string, unknown>
  } | null>(null)

  const run = useCallback(async (action: () => Promise<void>) => {
    setBusy(true)
    setError(null)
    try {
      await action()
    } catch (error) {
      setError(roundtableError(error))
    } finally {
      setBusy(false)
    }
  }, [])

  const listRooms = useCallback(
    async (cursor?: string) => {
      const page = await roundtableCall<{
        rooms: RoomSummary[]
        cursor: string | null
      }>("roundtable_list", {
        workspace_id: workspaceId,
        limit: 100,
        ...(cursor ? { cursor } : {}),
      })
      setRooms((previous) =>
        cursor ? [...previous, ...page.rooms] : page.rooms
      )
      setListCursor(page.cursor)
    },
    [workspaceId]
  )

  useEffect(() => {
    let live = true
    Promise.all([listModelProviders(), listRooms()])
      .then(([items]) => {
        if (live) setProviders(items)
      })
      .catch((error) => {
        if (live) setError(roundtableError(error))
      })
    return () => {
      live = false
    }
  }, [listRooms])

  useEffect(() => {
    if (!roomId) return
    let live = true
    let syncing = false
    let previous: RoundtableProjection | null = null
    let unsubscribe: (() => void) | undefined
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
          setError(null)
        }
      } catch (error) {
        previous = null
        if (live) {
          setPreviews({})
          setError(roundtableError(error))
        }
      } finally {
        syncing = false
      }
    }
    const reconnect = transport.onReconnect?.(() => {
      previous = null
      setPreviews({})
      void attach()
        .then(sync)
        .catch((error) => {
          if (live) setError(roundtableError(error))
        })
    })
    void (async () => {
      unsubscribe = await transport.subscribe(
        `roundtable://${subscriptionId}`,
        (payload: unknown) => {
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
      await sync()
    })().catch((error) => {
      if (live) setError(roundtableError(error))
    })
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
        ...(original?.participants[ordinal]?.model &&
        original.participants[ordinal].provider_ref ===
          `provider:${providerIds[ordinal]}`
          ? { model: original.participants[ordinal].model }
          : {}),
        ...(original?.participants[ordinal]?.effort &&
        original.participants[ordinal].provider_ref ===
          `provider:${providerIds[ordinal]}`
          ? { effort: original.participants[ordinal].effort }
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
  const configKey = JSON.stringify([
    roomId,
    loaded?.projection.body.revision,
    config,
  ])
  const invalidate = () => {
    setPreflight(null)
    setConfirmed(false)
    setRecoveryConsent(false)
  }
  const check = () =>
    run(async () => {
      const result = await roundtableCall<RoundtablePreflight>(
        "roundtable_preflight",
        {
          config,
          ...(roomId
            ? { room_id: roomId, revision: loaded?.projection.body.revision }
            : {}),
        }
      )
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
      const base = roomId
        ? {
            room_id: roomId,
            expected_revision: loaded?.projection.body.revision,
            ...extra,
          }
        : extra
      const key = JSON.stringify([command, roomId, extra])
      if (mutation.current?.key !== key)
        mutation.current = {
          key,
          request: { ...base, request_id: crypto.randomUUID() },
        }
      const ack = await roundtableCall<{
        room_id: string
        operation_id: string | null
      }>(command, mutation.current.request).catch((error: unknown) => {
        if (
          error &&
          typeof error === "object" &&
          "code" in error &&
          [
            "invalid_argument",
            "revision_conflict",
            "forbidden",
            "capacity_limited",
            "insufficient_budget",
          ].includes(String(error.code))
        )
          mutation.current = null
        throw error
      })
      mutation.current = null
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
    preflight?.enabled === true &&
    preflight.readiness === "ready" &&
    !preflight.error &&
    confirmed &&
    preflightKey === configKey &&
    !editingDraft
  const projection = loaded?.projection
  const status = projection?.body.status
  const buttonClass = "justify-start"
  const editDraft = () => {
    setTopic(config.topic)
    setRoles(config.participants.map((member) => member.role))
    setProviderIds(
      config.participants.map((member) =>
        member.provider_ref.replace(/^provider:/, "")
      )
    )
    setRounds(config.strategy.critique_rounds)
    setConcurrency(config.concurrency)
    setModerator(config.moderator_ordinal)
    setEditingDraft(true)
    invalidate()
  }

  return (
    <main className="mx-auto flex max-w-5xl flex-col gap-5 overflow-auto p-6">
      <header className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold">{t("title")}</h1>
        <Link href="/workspace">{t("back")}</Link>
      </header>
      {error ? (
        <p role="alert" className="text-destructive">
          {error}
        </p>
      ) : null}
      {!workspaceId ? <p role="alert">{t("workspaceRequired")}</p> : null}
      <div className="grid gap-6 md:grid-cols-[15rem_1fr]">
        <aside className="flex flex-col gap-2">
          <Link
            href={`/roundtable?workspace_id=${encodeURIComponent(workspaceId)}`}
          >
            {t("new")}
          </Link>
          <Button variant="outline" onClick={() => void run(() => listRooms())}>
            {t("refresh")}
          </Button>
          <ul>
            {rooms.map((room) => (
              <li key={room.room_id} className="py-2">
                <Link
                  href={`/roundtable?workspace_id=${encodeURIComponent(workspaceId)}&room_id=${encodeURIComponent(room.room_id)}`}
                >
                  {room.config.topic}
                  <span className="block text-xs text-muted-foreground">
                    {room.status}
                  </span>
                </Link>
              </li>
            ))}
          </ul>
          {listCursor ? (
            <Button
              variant="outline"
              onClick={() => void run(() => listRooms(listCursor))}
            >
              {t("more")}
            </Button>
          ) : null}
        </aside>
        <section className="flex flex-col gap-4">
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
              {roles.map((role, index) => (
                <fieldset
                  key={index}
                  className="grid gap-2 rounded-lg border p-3"
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
              <div className="flex gap-2">
                <Button
                  variant="outline"
                  disabled={roles.length >= 7}
                  onClick={() => {
                    setRoles([...roles, ""])
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
              <h2 className="text-xl">{config.topic}</h2>
              <p>
                {t("status")}: {status}
              </p>
              {projection.body.blocked_reason ? (
                <p role="status">{projection.body.blocked_reason}</p>
              ) : null}
              <ol>
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
              {Object.entries(previews)
                .filter(([, view]) => view.preview !== null)
                .map(([attemptId, view]) => (
                  <RoundtableSafeContent key={attemptId} text={view.preview!} />
                ))}
              <section
                aria-label={t("results")}
                className="flex flex-col gap-3"
              >
                {loaded?.messages
                  .filter((message) => message.visibility !== "void")
                  .map((message) => (
                    <div
                      key={message.message_id}
                      className="rounded-lg border p-3"
                    >
                      <p className="text-xs text-muted-foreground">
                        {message.visibility === "published"
                          ? t("published")
                          : t("staged")}
                      </p>
                      <RoundtableSafeContent
                        text={
                          message.body.summary ??
                          message.body.recommendation?.text ??
                          JSON.stringify(message.body)
                        }
                        preview={false}
                      />
                      <details>
                        <summary>{t("details")}</summary>
                        <RoundtableSafeContent
                          text={JSON.stringify(message.body, null, 2)}
                          preview={false}
                        />
                      </details>
                    </div>
                  ))}
              </section>
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
          ) : (
            <p role="status">{t("loading")}</p>
          )}
          <Button
            variant="outline"
            disabled={
              busy ||
              !workspaceId ||
              editingDraft ||
              (!roomId && (!topic.trim() || providers.length === 0))
            }
            onClick={() => void check()}
          >
            {t("preflight")}
          </Button>
          {preflight ? (
            <>
              <p role="status">
                {preflight.enabled ? t("enabled") : t("disabled")}
              </p>
              {preflight.error ? (
                <p role="alert">{roundtableError(preflight.error)}</p>
              ) : null}
              <PreflightConfirmation
                targets={config.participants.map((member) => member.role)}
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
                onClick={() => void mutate("roundtable_create", { config })}
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
                    onClick={() => void mutate("roundtable_retry_synthesis")}
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
                  disabled={busy}
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
                disabled={busy || !interjection.trim()}
                onClick={() =>
                  void mutate("roundtable_interject", {
                    text: interjection,
                    mode: interjectMode,
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
