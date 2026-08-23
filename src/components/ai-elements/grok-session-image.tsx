"use client"

import { useEffect, useRef, useState, type ReactElement } from "react"
import { useWorkspaceActions } from "@/contexts/workspace-context"
import { extractAppCommandError } from "@/lib/app-error"
import { resolveGrokSessionImage } from "@/lib/api"
import { isAbsoluteFilePath } from "@/lib/file-path-display"
import {
  GROK_SESSION_IMAGE_MIME_BY_EXTENSION,
  parseGrokSessionImageRef,
} from "@/lib/markdown/grok-session-image"
import type {
  GrokSessionImageMimeType,
  GrokSessionImageResolution,
} from "@/lib/types"
import {
  useGrokSessionImageScope,
  type GrokSessionImagePhase,
} from "./grok-session-image-context"

export const GROK_IMAGE_ATTEMPT_DUE_MS = [0, 400, 1_200, 2_500] as const

export type GrokSessionImageProps = {
  src?: string
  alt?: string
}

type ValidatedGrokSessionImageResolution = Omit<
  GrokSessionImageResolution,
  "dataBase64"
> & { dataBase64: string }

type AttemptControl = {
  generation: number
  startedAt: number
  attemptsStarted: number
  nextDeadlineIndex: number
  inFlightAttempt: number | null
  displayedAttempt: number | null
  timer: ReturnType<typeof setTimeout> | null
  deadlineElapsed: boolean
  retryEligible: "not-found" | "workspace" | "decode" | null
  phase: GrokSessionImagePhase
  disposed: boolean
}

type GrokSessionImageViewState =
  | {
      identityKey: string | null
      status: "loading" | "failed"
      resolution: null
      attempt: null
      decoded: false
    }
  | {
      identityKey: string
      status: "ready"
      resolution: ValidatedGrokSessionImageResolution
      attempt: number
      decoded: boolean
    }

type RenderState = {
  generation: number
  view: GrokSessionImageViewState
}

type AttemptRunner = {
  identityKey: string | null
  control: AttemptControl
  start: () => void
  onLoad: (
    generation: number,
    attempt: number,
    origin: ValidatedGrokSessionImageResolution["origin"]
  ) => void
  onError: (generation: number, attempt: number) => void
}

function loadingState(identityKey: string): GrokSessionImageViewState {
  return {
    identityKey,
    status: "loading",
    resolution: null,
    attempt: null,
    decoded: false,
  }
}

function failedState(identityKey: string | null): GrokSessionImageViewState {
  return {
    identityKey,
    status: "failed",
    resolution: null,
    attempt: null,
    decoded: false,
  }
}

function validateResolution(
  value: unknown,
  expectedMimeType: GrokSessionImageMimeType
): ValidatedGrokSessionImageResolution | null {
  if (value === null || typeof value !== "object") return null

  const record = value as Record<string, unknown>
  if (
    (record.origin !== "session" && record.origin !== "workspace") ||
    record.mimeType !== expectedMimeType ||
    typeof record.path !== "string" ||
    !isAbsoluteFilePath(record.path) ||
    typeof record.dataBase64 !== "string" ||
    record.dataBase64.trim().length === 0
  ) {
    return null
  }

  return {
    path: record.path,
    origin: record.origin,
    mimeType: expectedMimeType,
    dataBase64: record.dataBase64,
  }
}

function clearTimer(control: AttemptControl): void {
  if (control.timer !== null) {
    clearTimeout(control.timer)
    control.timer = null
  }
}

function disposeControl(control: AttemptControl): void {
  control.disposed = true
  clearTimer(control)
}

export function GrokSessionImage({
  src,
  alt,
}: GrokSessionImageProps): ReactElement {
  const scope = useGrokSessionImageScope()
  const { openResolvedImagePreview } = useWorkspaceActions()
  const parsed = typeof src === "string" ? parseGrokSessionImageRef(src) : null
  const identityKey =
    scope && parsed ? `${scope.conversationId}\0${parsed.path}` : null
  const label = alt?.trim() || parsed?.filename || "image"
  const phase = scope?.phase ?? null
  const conversationId = scope?.conversationId ?? null
  const extension = parsed?.extension ?? null

  const controlRef = useRef<AttemptControl>({
    generation: 0,
    startedAt: 0,
    attemptsStarted: 0,
    nextDeadlineIndex: 1,
    inFlightAttempt: null,
    displayedAttempt: null,
    timer: null,
    deadlineElapsed: false,
    retryEligible: null,
    phase: "complete",
    disposed: true,
  })
  const runnerRef = useRef<AttemptRunner | null>(null)
  const pendingRenderResetRef = useRef<RenderState | null>(null)
  const [renderState, setRenderState] = useState<RenderState>(() => ({
    generation: 0,
    view: identityKey ? loadingState(identityKey) : failedState(null),
  }))

  useEffect(() => {
    function createRunner(
      control: AttemptControl,
      runnerIdentityKey: string,
      runnerConversationId: number,
      requestHref: string,
      expectedMimeType: GrokSessionImageMimeType
    ): AttemptRunner {
      const generation = control.generation

      function isCurrent(): boolean {
        return (
          !control.disposed &&
          controlRef.current === control &&
          runnerRef.current?.control === control &&
          runnerRef.current.identityKey === runnerIdentityKey
        )
      }

      function settleView(view: GrokSessionImageViewState): void {
        if (!isCurrent()) return
        if (pendingRenderResetRef.current?.generation === generation) {
          pendingRenderResetRef.current = { generation, view }
          return
        }
        setRenderState((current) =>
          current.generation === generation ? { generation, view } : current
        )
      }

      function finalizeFailure(): void {
        if (!isCurrent()) return
        clearTimer(control)
        control.deadlineElapsed = false
        control.retryEligible = null
        control.inFlightAttempt = null
        control.displayedAttempt = null
        settleView(failedState(runnerIdentityKey))
      }

      function consumeElapsedDeadlines(
        target: AttemptControl,
        now: number
      ): boolean {
        let elapsed = target.deadlineElapsed
        let consumedArmedDeadline = false
        target.deadlineElapsed = false
        while (
          target.nextDeadlineIndex < GROK_IMAGE_ATTEMPT_DUE_MS.length &&
          target.startedAt +
            GROK_IMAGE_ATTEMPT_DUE_MS[target.nextDeadlineIndex] <=
            now
        ) {
          target.nextDeadlineIndex += 1
          elapsed = true
          consumedArmedDeadline = true
        }
        if (consumedArmedDeadline) clearTimer(target)
        return elapsed
      }

      function armNextDeadline(target: AttemptControl): void {
        if (
          !isCurrent() ||
          target.phase !== "live" ||
          target.timer !== null ||
          target.attemptsStarted >= GROK_IMAGE_ATTEMPT_DUE_MS.length ||
          target.nextDeadlineIndex >= GROK_IMAGE_ATTEMPT_DUE_MS.length
        ) {
          return
        }

        const deadlineIndex = target.nextDeadlineIndex
        const delay = Math.max(
          0,
          target.startedAt +
            GROK_IMAGE_ATTEMPT_DUE_MS[deadlineIndex] -
            Date.now()
        )
        target.timer = setTimeout(() => {
          if (!isCurrent()) return
          target.timer = null
          if (target.nextDeadlineIndex === deadlineIndex) {
            target.nextDeadlineIndex += 1
          }
          if (target.phase !== "live") return
          if (target.inFlightAttempt !== null) {
            target.deadlineElapsed = true
            return
          }
          if (
            target.retryEligible !== null &&
            target.attemptsStarted < GROK_IMAGE_ATTEMPT_DUE_MS.length
          ) {
            startAttempt(target)
            return
          }
          target.deadlineElapsed = true
        }, delay)
      }

      function startAttempt(target: AttemptControl): void {
        if (
          !isCurrent() ||
          target.inFlightAttempt !== null ||
          target.attemptsStarted >= GROK_IMAGE_ATTEMPT_DUE_MS.length
        ) {
          return
        }

        const attempt = target.attemptsStarted
        target.attemptsStarted += 1
        target.inFlightAttempt = attempt
        target.retryEligible = null
        armNextDeadline(target)

        let request: Promise<GrokSessionImageResolution>
        try {
          request = resolveGrokSessionImage({
            conversationId: runnerConversationId,
            href: requestHref,
            includeData: true,
          })
        } catch (error) {
          settleRejected(target, attempt, error)
          return
        }

        void request.then(
          (value: unknown) => settleResolved(target, attempt, value),
          (error: unknown) => settleRejected(target, attempt, error)
        )
      }

      function requestEligibleRetry(target: AttemptControl): boolean {
        if (
          target.phase !== "live" ||
          target.disposed ||
          target.attemptsStarted >= GROK_IMAGE_ATTEMPT_DUE_MS.length
        ) {
          return false
        }
        if (target.inFlightAttempt !== null) return false
        if (consumeElapsedDeadlines(target, Date.now())) {
          startAttempt(target)
          return true
        }
        armNextDeadline(target)
        return false
      }

      function hasPendingRetry(target: AttemptControl): boolean {
        return (
          target.phase === "live" &&
          (target.inFlightAttempt !== null ||
            target.timer !== null ||
            target.deadlineElapsed)
        )
      }

      function showRetryWait(): void {
        if (!isCurrent()) return
        setRenderState((current) => {
          if (current.generation !== generation) return current
          if (
            current.view.identityKey === runnerIdentityKey &&
            current.view.status === "ready" &&
            current.view.decoded &&
            current.view.resolution.origin === "workspace"
          ) {
            return current
          }
          return {
            generation,
            view: loadingState(runnerIdentityKey),
          }
        })
      }

      function settleResolved(
        target: AttemptControl,
        attempt: number,
        value: unknown
      ): void {
        if (!isCurrent() || target.inFlightAttempt !== attempt) return
        const validated = validateResolution(value, expectedMimeType)
        if (!validated) {
          finalizeFailure()
          return
        }

        target.displayedAttempt = attempt
        target.retryEligible =
          validated.origin === "workspace" ? "workspace" : null
        settleView({
          identityKey: runnerIdentityKey,
          status: "ready",
          resolution: validated,
          attempt,
          decoded: false,
        })
      }

      function settleRejected(
        target: AttemptControl,
        attempt: number,
        error: unknown
      ): void {
        if (!isCurrent() || target.inFlightAttempt !== attempt) return
        target.inFlightAttempt = null
        if (extractAppCommandError(error)?.code !== "not_found") {
          finalizeFailure()
          return
        }

        target.retryEligible = "not-found"
        requestEligibleRetry(target)
        if (hasPendingRetry(target)) {
          showRetryWait()
        } else {
          finalizeFailure()
        }
      }

      const runner: AttemptRunner = {
        identityKey: runnerIdentityKey,
        control,
        start() {
          startAttempt(control)
        },
        onLoad(capturedGeneration, attempt, origin) {
          if (
            capturedGeneration !== generation ||
            !isCurrent() ||
            control.inFlightAttempt !== attempt ||
            control.displayedAttempt !== attempt
          ) {
            return
          }

          control.inFlightAttempt = null
          control.retryEligible = origin === "workspace" ? "workspace" : null
          setRenderState((current) => {
            if (
              current.generation !== generation ||
              current.view.status !== "ready" ||
              current.view.identityKey !== runnerIdentityKey ||
              current.view.attempt !== attempt
            ) {
              return current
            }
            return {
              generation,
              view: { ...current.view, decoded: true },
            }
          })

          if (origin === "session") {
            clearTimer(control)
            control.deadlineElapsed = false
            control.nextDeadlineIndex = GROK_IMAGE_ATTEMPT_DUE_MS.length
            return
          }
          requestEligibleRetry(control)
        },
        onError(capturedGeneration, attempt) {
          if (
            capturedGeneration !== generation ||
            !isCurrent() ||
            control.inFlightAttempt !== attempt ||
            control.displayedAttempt !== attempt
          ) {
            return
          }

          control.inFlightAttempt = null
          control.displayedAttempt = null
          control.retryEligible = "decode"
          requestEligibleRetry(control)
          if (hasPendingRetry(control)) {
            showRetryWait()
          } else {
            finalizeFailure()
          }
        },
      }
      return runner
    }

    const currentRunner = runnerRef.current
    if (currentRunner?.identityKey !== identityKey) {
      if (currentRunner) disposeControl(currentRunner.control)

      const generation = controlRef.current.generation + 1
      const control: AttemptControl = {
        generation,
        startedAt: Date.now(),
        attemptsStarted: 0,
        nextDeadlineIndex: 1,
        inFlightAttempt: null,
        displayedAttempt: null,
        timer: null,
        deadlineElapsed: false,
        retryEligible: null,
        phase: phase ?? "complete",
        disposed: identityKey === null,
      }
      controlRef.current = control

      if (
        identityKey === null ||
        phase === null ||
        conversationId === null ||
        extension === null ||
        typeof src !== "string"
      ) {
        runnerRef.current = {
          identityKey: null,
          control,
          start: () => {},
          onLoad: () => {},
          onError: () => {},
        }
        pendingRenderResetRef.current = {
          generation,
          view: failedState(null),
        }
        return
      }

      const runner = createRunner(
        control,
        identityKey,
        conversationId,
        src,
        GROK_SESSION_IMAGE_MIME_BY_EXTENSION[extension]
      )
      runnerRef.current = runner
      pendingRenderResetRef.current = {
        generation,
        view: loadingState(identityKey),
      }
      // The initial attempt is due at zero and never enters the timer queue.
      runner.start()
      return
    }

    if (
      !currentRunner ||
      phase === null ||
      identityKey === null ||
      conversationId === null ||
      extension === null ||
      typeof src !== "string"
    ) {
      return
    }
    const control = currentRunner.control
    if (control.phase === phase) return

    if (control.phase === "complete" && phase === "live") {
      disposeControl(control)
      const generation = control.generation + 1
      const freshControl: AttemptControl = {
        generation,
        startedAt: Date.now(),
        attemptsStarted: 0,
        nextDeadlineIndex: 1,
        inFlightAttempt: null,
        displayedAttempt: null,
        timer: null,
        deadlineElapsed: false,
        retryEligible: null,
        phase,
        disposed: false,
      }
      controlRef.current = freshControl
      const freshRunner = createRunner(
        freshControl,
        identityKey,
        conversationId,
        src,
        GROK_SESSION_IMAGE_MIME_BY_EXTENSION[extension]
      )
      runnerRef.current = freshRunner
      pendingRenderResetRef.current = {
        generation,
        view: loadingState(identityKey),
      }
      freshRunner.start()
      return
    }

    control.phase = "complete"
    clearTimer(control)
    control.deadlineElapsed = false
    control.retryEligible = null
    if (control.inFlightAttempt === null && control.displayedAttempt === null) {
      pendingRenderResetRef.current = {
        generation: control.generation,
        view: failedState(identityKey),
      }
    }
  }, [conversationId, extension, identityKey, phase, src])

  useEffect(() => {
    const pending = pendingRenderResetRef.current
    if (pending === null) return
    pendingRenderResetRef.current = null
    // A generation/phase boundary deliberately synchronizes visible state.
    setRenderState(pending)
  }, [identityKey, phase])

  useEffect(
    () => () => {
      const runner = runnerRef.current
      if (runner) disposeControl(runner.control)
      runnerRef.current = null
    },
    []
  )

  const view =
    identityKey === null
      ? failedState(null)
      : renderState.view.identityKey === identityKey
        ? renderState.view
        : loadingState(identityKey)

  if (view.status !== "ready") {
    return (
      <span
        aria-busy={view.status === "loading" ? "true" : undefined}
        className={
          view.status === "failed" ? "text-muted-foreground text-sm" : "text-sm"
        }
      >
        {label}
      </span>
    )
  }

  const { resolution, attempt, decoded } = view
  const renderGeneration = renderState.generation
  return (
    <button
      type="button"
      aria-label={label}
      className="block max-w-full overflow-hidden rounded-md"
      onClick={() => {
        const runner = runnerRef.current
        if (
          !decoded ||
          !runner ||
          runner.identityKey !== identityKey ||
          runner.control.disposed ||
          runner.control.generation !== renderGeneration ||
          runner.control.displayedAttempt !== attempt ||
          !scope ||
          !parsed
        ) {
          return
        }
        openResolvedImagePreview({
          path: resolution.path,
          mimeType: resolution.mimeType,
          dataBase64: resolution.dataBase64,
          source: {
            type: "grok-session-image",
            conversationId: scope.conversationId,
            href: parsed.path,
          },
        })
      }}
    >
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        key={`${identityKey}:${attempt}`}
        src={`data:${resolution.mimeType};base64,${resolution.dataBase64}`}
        alt={label}
        className="max-h-80 max-w-full object-contain"
        onLoad={() =>
          runnerRef.current?.onLoad(
            renderGeneration,
            attempt,
            resolution.origin
          )
        }
        onError={() => runnerRef.current?.onError(renderGeneration, attempt)}
      />
    </button>
  )
}
