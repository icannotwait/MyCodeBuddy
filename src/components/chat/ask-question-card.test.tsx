import { act, cleanup, fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"

import { AskQuestionCard } from "./ask-question-card"
import arMessages from "@/i18n/messages/ar.json"
import deMessages from "@/i18n/messages/de.json"
import enMessages from "@/i18n/messages/en.json"
import esMessages from "@/i18n/messages/es.json"
import frMessages from "@/i18n/messages/fr.json"
import jaMessages from "@/i18n/messages/ja.json"
import koMessages from "@/i18n/messages/ko.json"
import ptMessages from "@/i18n/messages/pt.json"
import zhCNMessages from "@/i18n/messages/zh-CN.json"
import zhTWMessages from "@/i18n/messages/zh-TW.json"
import type { PendingQuestionState, QuestionAnswer } from "@/lib/types"

function renderCard(
  question: PendingQuestionState,
  onAnswer = vi.fn(),
  extra: { interactionLocked?: boolean; readOnly?: boolean } = {}
) {
  render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <AskQuestionCard
        question={question}
        onAnswer={onAnswer}
        interactionLocked={extra.interactionLocked}
        readOnly={extra.readOnly}
      />
    </NextIntlClientProvider>
  )
  return onAnswer
}

/** Render with an explicit (typically async) `onAnswer`, returning the render
 *  result so a test can reach into `container` for the spinner. */
function renderWith(
  question: PendingQuestionState,
  onAnswer: (questionId: string, answer: QuestionAnswer) => void | Promise<void>
) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <AskQuestionCard question={question} onAnswer={onAnswer} />
    </NextIntlClientProvider>
  )
}

/** A manually-resolvable promise so a test can hold the answer round-trip
 *  "in flight" and assert the card's disabled/spinner state. */
function deferred() {
  let resolve!: () => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<void>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

const single: PendingQuestionState = {
  question_id: "q-1",
  created_at: "2026-01-01T00:00:00Z",
  questions: [
    {
      id: "qa",
      question: "Which approach?",
      header: "Approach",
      multi_select: false,
      options: [
        { label: "Incremental", description: "smaller diffs" },
        { label: "Rewrite", description: "clean slate" },
      ],
    },
  ],
}

const multi: PendingQuestionState = {
  question_id: "q-2",
  created_at: "2026-01-01T00:00:00Z",
  questions: [
    {
      id: "qb",
      question: "Which modules?",
      header: "Scope",
      multi_select: true,
      options: [
        { label: "auth", description: "" },
        { label: "billing", description: "" },
        { label: "ui", description: "" },
      ],
    },
  ],
}

// Two single-select questions — exercises the tabbed multi-question layout.
const twoSingle: PendingQuestionState = {
  question_id: "q-two",
  created_at: "2026-01-01T00:00:00Z",
  questions: [
    {
      id: "qa",
      question: "First question?",
      header: "First",
      multi_select: false,
      options: [
        { label: "X", description: "" },
        { label: "Y", description: "" },
      ],
    },
    {
      id: "qb",
      question: "Second question?",
      header: "Second",
      multi_select: false,
      options: [
        { label: "P", description: "" },
        { label: "Q", description: "" },
      ],
    },
  ],
}

// First question is multi-select — used to assert it does NOT auto-advance.
const twoMultiFirst: PendingQuestionState = {
  question_id: "q-two-multi",
  created_at: "2026-01-01T00:00:00Z",
  questions: [
    {
      id: "qa",
      question: "First question?",
      header: "First",
      multi_select: true,
      options: [
        { label: "X", description: "" },
        { label: "Y", description: "" },
      ],
    },
    {
      id: "qb",
      question: "Second question?",
      header: "Second",
      multi_select: false,
      options: [
        { label: "P", description: "" },
        { label: "Q", description: "" },
      ],
    },
  ],
}

describe("AskQuestionCard", () => {
  it("localizes a recovery card from codes and submits raw approve or decline", async () => {
    const actions = [
      "continue",
      "fresh_dispatch",
      "replace",
      "recover_workflow",
      "reset_plan_lineage",
    ] as const
    const causes = [
      "completed",
      "revision_eligible_failure",
      "unexpected_transport_loss",
      "unexpected_process_loss",
      "unexpected_session_loss",
      "unexpected_host_restart",
      "unexpected_child_connection_loss",
      "parent_canceled",
      "parent_turn_failed",
      "join_abandoned",
      "user_cancelled",
      "tool_stalled_timeout",
      "legacy_parent_disconnect",
      "intentional_parent_disconnect",
      "malformed_termination_audit",
      "pre_admission_retry",
      "pre_admission_abort",
      "admission_failed",
      "admission_unknown",
      "missing_resume_identity",
      "unsupported_reuse",
      "persisted_unresumable",
      "continue_budget_exhausted",
      "replacement_budget_exhausted",
      "route_rejected",
      "stale_source",
      "busy_source",
      "structural_fence",
      "contradictory_evidence",
      "legacy_block_with_current_plan_approval",
      "legacy_block_with_current_plan",
      "legacy_block_without_plan",
      "plan_user_decision_required",
      "plan_gate_blocked",
      "explicit_manifest_block",
      "unresolved_task_cohort",
      "durable_state_inconsistent",
    ] as const
    const risks = [
      "normal",
      "execution_may_have_occurred",
      "explicit_user_stop",
      "legacy_unknown_origin",
      "plan_lineage_reset",
      "durable_state_risk",
    ] as const
    const subjects = ["delegation_task", "workflow"] as const
    const locales = [
      ["en", enMessages],
      ["ar", arMessages],
      ["de", deMessages],
      ["es", esMessages],
      ["fr", frMessages],
      ["ja", jaMessages],
      ["ko", koMessages],
      ["pt", ptMessages],
      ["zh-CN", zhCNMessages],
      ["zh-TW", zhTWMessages],
    ] as const

    const recoveryQuestion = (
      action: (typeof actions)[number],
      cause: (typeof causes)[number],
      risk: (typeof risks)[number],
      subject: (typeof subjects)[number]
    ): PendingQuestionState => ({
      question_id: `recovery-${action}-${cause}-${risk}-${subject}`,
      created_at: "2026-01-01T00:00:00Z",
      questions: [
        {
          id: "recovery-choice",
          question: "MODEL-PROVIDED QUESTION MUST NOT RENDER",
          header: "MODEL HEADER",
          multi_select: false,
          options: [
            {
              label: "approve",
              description: "MODEL APPROVE COPY MUST NOT RENDER",
            },
            {
              label: "decline",
              description: "MODEL DECLINE COPY MUST NOT RENDER",
            },
          ],
          recovery: {
            subject,
            action,
            target: "MODEL TARGET MUST NOT RENDER",
            cause,
            risk,
            display_reason: "MODEL REASON MUST NOT RENDER",
          },
        },
      ],
    })

    for (const [locale, messages] of locales) {
      for (let index = 0; index < causes.length; index += 1) {
        const action = actions[index % actions.length]
        const risk = risks[index % risks.length]
        const subject = subjects[index % subjects.length]
        const onAnswer = vi.fn().mockResolvedValue(undefined)
        render(
          <NextIntlClientProvider locale={locale} messages={messages}>
            <AskQuestionCard
              question={recoveryQuestion(action, causes[index], risk, subject)}
              onAnswer={onAnswer}
            />
          </NextIntlClientProvider>
        )

        if (locale === "en") {
          expect(
            screen.getByRole("group", {
              name: "Recovery confirmation required",
            })
          ).toBeInTheDocument()
        }

        expect(
          screen.getByRole("group", {
            name: messages.Folder.chat.askQuestion.recovery.title,
          })
        ).toBeInTheDocument()
        expect(
          screen.getByText(
            messages.Folder.chat.askQuestion.recovery.actions[action]
          )
        ).toBeInTheDocument()
        expect(
          screen.getByText(
            messages.Folder.chat.askQuestion.recovery.causes[causes[index]]
          )
        ).toBeInTheDocument()
        expect(
          screen.getByText(
            messages.Folder.chat.askQuestion.recovery.risks[risk]
          )
        ).toBeInTheDocument()
        expect(
          screen.getByText(
            messages.Folder.chat.askQuestion.recovery.subjects[subject]
          )
        ).toBeInTheDocument()
        for (const modelCopy of [
          "MODEL-PROVIDED QUESTION MUST NOT RENDER",
          "MODEL HEADER",
          "MODEL APPROVE COPY MUST NOT RENDER",
          "MODEL DECLINE COPY MUST NOT RENDER",
          "MODEL TARGET MUST NOT RENDER",
          "MODEL REASON MUST NOT RENDER",
        ]) {
          expect(screen.queryByText(modelCopy)).not.toBeInTheDocument()
        }
        expect(screen.queryByRole("radio")).not.toBeInTheDocument()
        expect(screen.queryByRole("textbox")).not.toBeInTheDocument()
        expect(
          screen.queryByRole("button", {
            name: messages.Folder.chat.askQuestion.skip,
          })
        ).not.toBeInTheDocument()
        cleanup()
      }
    }

    const approve = deferred()
    const onApprove = vi.fn().mockReturnValue(approve.promise)
    const approveQuestion = recoveryQuestion(
      "continue",
      "parent_canceled",
      "explicit_user_stop",
      "delegation_task"
    )
    const { rerender } = render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <AskQuestionCard question={approveQuestion} onAnswer={onApprove} />
      </NextIntlClientProvider>
    )
    const approveButton = screen.getByRole("button", {
      name: enMessages.Folder.chat.askQuestion.recovery.approve,
    })
    const declineButton = screen.getByRole("button", {
      name: enMessages.Folder.chat.askQuestion.recovery.decline,
    })
    fireEvent.click(approveButton)
    expect(onApprove).toHaveBeenCalledWith(approveQuestion.question_id, {
      answers: [{ questionId: "recovery-choice", labels: ["approve"] }],
      declined: false,
    })
    expect(approveButton).toBeDisabled()
    expect(declineButton).toBeDisabled()
    approve.resolve()
    await act(async () => {
      await approve.promise
    })
    const replacementQuestion = recoveryQuestion(
      "replace",
      "tool_stalled_timeout",
      "execution_may_have_occurred",
      "delegation_task"
    )
    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <AskQuestionCard question={replacementQuestion} onAnswer={vi.fn()} />
      </NextIntlClientProvider>
    )
    expect(
      screen.getByRole("button", {
        name: enMessages.Folder.chat.askQuestion.recovery.approve,
      })
    ).toBeEnabled()
    expect(
      screen.getByRole("button", {
        name: enMessages.Folder.chat.askQuestion.recovery.decline,
      })
    ).toBeEnabled()
    cleanup()

    const onDecline = vi.fn().mockResolvedValue(undefined)
    const declineQuestion = recoveryQuestion(
      "recover_workflow",
      "plan_gate_blocked",
      "normal",
      "workflow"
    )
    render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <AskQuestionCard question={declineQuestion} onAnswer={onDecline} />
      </NextIntlClientProvider>
    )
    fireEvent.click(
      screen.getByRole("button", {
        name: enMessages.Folder.chat.askQuestion.recovery.decline,
      })
    )
    expect(onDecline).toHaveBeenCalledWith(declineQuestion.question_id, {
      answers: [{ questionId: "recovery-choice", labels: ["decline"] }],
      declined: true,
    })
    cleanup()

    for (const mutation of [
      { action: "unknown_action" },
      { cause: "unknown_cause" },
      { risk: "unknown_risk" },
      { subject: "unknown_subject" },
    ]) {
      const unknown = recoveryQuestion(
        "continue",
        "parent_canceled",
        "normal",
        "delegation_task"
      )
      Object.assign(unknown.questions[0].recovery!, mutation)
      const { container } = render(
        <NextIntlClientProvider locale="en" messages={enMessages}>
          <AskQuestionCard question={unknown} onAnswer={vi.fn()} />
        </NextIntlClientProvider>
      )
      expect(container).toBeEmptyDOMElement()
      cleanup()
    }
  })

  it("keeps generic ask_user_question behavior unchanged", () => {
    const onAnswer = renderCard(single)
    expect(screen.getByText("Which approach?")).toBeInTheDocument()
    expect(screen.getByText("smaller diffs")).toBeInTheDocument()
    expect(screen.getByText("Other")).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "Skip" })).toBeInTheDocument()
    expect(screen.queryByText(/recovery/i)).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole("radio", { name: /Incremental/ }))
    fireEvent.click(screen.getByRole("button", { name: "Submit" }))
    expect(onAnswer).toHaveBeenCalledWith("q-1", {
      answers: [{ questionId: "qa", labels: ["Incremental"] }],
      declined: false,
    })
  })

  it("submits a single-select choice keyed by question id", () => {
    const onAnswer = renderCard(single)
    fireEvent.click(screen.getByRole("radio", { name: /Incremental/ }))
    fireEvent.click(screen.getByRole("button", { name: "Submit" }))
    expect(onAnswer).toHaveBeenCalledWith("q-1", {
      answers: [{ questionId: "qa", labels: ["Incremental"] }],
      declined: false,
    })
  })

  it("interactionLocked disables option/submit mutations while keeping the card visible", () => {
    const onAnswer = renderCard(single, vi.fn(), { interactionLocked: true })
    expect(screen.getByText("Which approach?")).toBeInTheDocument()
    const radio = screen.getByRole("radio", { name: /Incremental/ })
    expect(radio).toBeDisabled()
    fireEvent.click(radio)
    const submit = screen.getByRole("button", { name: "Submit" })
    expect(submit).toBeDisabled()
    fireEvent.click(submit)
    expect(onAnswer).not.toHaveBeenCalled()
  })

  it("interactionLocked guards Skip/Next/Submit so fireEvent cannot stick submitting", async () => {
    // fireEvent still invokes handlers on disabled buttons; run() must no-op
    // before setSubmitting so a locked Skip never latches the spinner forever.
    const onAnswer = vi.fn().mockResolvedValue(undefined)
    const { rerender } = render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <AskQuestionCard question={twoSingle} onAnswer={onAnswer} />
      </NextIntlClientProvider>
    )
    // First tab of a multi-question set exposes Next without answering.
    expect(screen.getByRole("button", { name: /Next/ })).toBeInTheDocument()

    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <AskQuestionCard
          question={twoSingle}
          onAnswer={onAnswer}
          interactionLocked
        />
      </NextIntlClientProvider>
    )

    const skip = screen.getByRole("button", { name: "Skip" })
    const next = screen.getByRole("button", { name: /Next/ })
    const submit = screen.getByRole("button", { name: /Submit/ })
    expect(skip).toBeDisabled()
    expect(next).toBeDisabled()
    expect(submit).toBeDisabled()

    fireEvent.click(skip)
    fireEvent.click(next)
    fireEvent.click(submit)
    await Promise.resolve()

    expect(onAnswer).not.toHaveBeenCalled()
    // Still locked-disabled (not permanently stuck in submitting spinner).
    expect(
      screen
        .queryByRole("button", { name: /Submit/ })
        ?.querySelector(".animate-spin")
    ).toBeNull()
  })

  it("disables Submit until something is selected", () => {
    renderCard(single)
    const submit = screen.getByRole("button", { name: "Submit" })
    expect(submit).toBeDisabled()
    fireEvent.click(screen.getByRole("radio", { name: /Rewrite/ }))
    expect(submit).not.toBeDisabled()
  })

  it("clears a single-select choice when the chosen option is clicked again", () => {
    renderCard(single)
    const submit = screen.getByRole("button", { name: "Submit" })
    fireEvent.click(screen.getByRole("radio", { name: /Incremental/ }))
    expect(submit).not.toBeDisabled()
    // Re-clicking the already-selected option deselects it (radix won't fire
    // onValueChange for the same value, so the card handles this via onClick).
    fireEvent.click(screen.getByRole("radio", { name: /Incremental/ }))
    expect(submit).toBeDisabled()
  })

  it("collects multiple labels in multi-select", () => {
    const onAnswer = renderCard(multi)
    fireEvent.click(screen.getByRole("checkbox", { name: "auth" }))
    fireEvent.click(screen.getByRole("checkbox", { name: "billing" }))
    fireEvent.click(screen.getByRole("button", { name: "Submit" }))
    expect(onAnswer).toHaveBeenCalledWith("q-2", {
      answers: [{ questionId: "qb", labels: ["auth", "billing"] }],
      declined: false,
    })
  })

  it("renders radio controls for single-select and checkboxes for multi-select", () => {
    const { unmount } = render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <AskQuestionCard question={single} onAnswer={vi.fn()} />
      </NextIntlClientProvider>
    )
    // Two real options + the host-injected "Other" row, all radios.
    expect(screen.getAllByRole("radio")).toHaveLength(3)
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument()
    unmount()

    renderCard(multi)
    // Three real options + "Other", all checkboxes.
    expect(screen.getAllByRole("checkbox")).toHaveLength(4)
    expect(screen.queryByRole("radio")).not.toBeInTheDocument()
  })

  it("submits the typed Other text as the answer label", () => {
    const onAnswer = renderCard(single)
    fireEvent.click(screen.getByRole("radio", { name: "Other" }))
    fireEvent.change(screen.getByPlaceholderText("Type your answer…"), {
      target: { value: "a third way" },
    })
    fireEvent.click(screen.getByRole("button", { name: "Submit" }))
    expect(onAnswer).toHaveBeenCalledWith("q-1", {
      answers: [{ questionId: "qa", labels: ["a third way"] }],
      declined: false,
    })
  })

  it("single-select Other replaces a prior option choice", () => {
    const onAnswer = renderCard(single)
    fireEvent.click(screen.getByRole("radio", { name: /Incremental/ }))
    fireEvent.click(screen.getByRole("radio", { name: "Other" }))
    fireEvent.change(screen.getByPlaceholderText("Type your answer…"), {
      target: { value: "custom" },
    })
    fireEvent.click(screen.getByRole("button", { name: "Submit" }))
    expect(onAnswer).toHaveBeenCalledWith("q-1", {
      answers: [{ questionId: "qa", labels: ["custom"] }],
      declined: false,
    })
  })

  it("renders a free-text question (no options) as a bare input and submits it", () => {
    // Codex elicitation / MCP-server forms ask open questions with 0 options:
    // the input is the answer field — no "Other" toggle to click through.
    const freeText: PendingQuestionState = {
      question_id: "q-free",
      created_at: "2026-01-01T00:00:00Z",
      questions: [
        {
          id: "qf",
          question: "What is the base URL?",
          header: "URL",
          multi_select: false,
          options: [],
        },
      ],
    }
    const onAnswer = renderCard(freeText)
    expect(screen.queryByRole("radio")).toBeNull()
    const input = screen.getByPlaceholderText("Type your answer…")
    const submit = screen.getByRole("button", { name: "Submit" })
    expect(submit).toBeDisabled()
    fireEvent.change(input, { target: { value: "https://api.example.com" } })
    expect(submit).toBeEnabled()
    fireEvent.click(submit)
    expect(onAnswer).toHaveBeenCalledWith("q-free", {
      answers: [{ questionId: "qf", labels: ["https://api.example.com"] }],
      declined: false,
    })
  })

  it("masks the input for a secret question", () => {
    const secret: PendingQuestionState = {
      question_id: "q-secret",
      created_at: "2026-01-01T00:00:00Z",
      questions: [
        {
          id: "qs",
          question: "Paste your API key",
          header: "Key",
          multi_select: false,
          options: [],
          is_secret: true,
        },
      ],
    }
    renderCard(secret)
    const input = screen.getByPlaceholderText("Type your answer…")
    expect(input).toHaveAttribute("type", "password")
  })

  it("skips with a declined answer", () => {
    const onAnswer = renderCard(single)
    fireEvent.click(screen.getByRole("button", { name: "Skip" }))
    expect(onAnswer).toHaveBeenCalledWith("q-1", {
      answers: [],
      declined: true,
    })
  })

  it("disables controls and shows a spinner while answering is in flight", () => {
    const d = deferred()
    const onAnswer = vi.fn(() => d.promise)
    const { container } = renderWith(single, onAnswer)
    fireEvent.click(screen.getByRole("radio", { name: /Incremental/ }))
    fireEvent.click(screen.getByRole("button", { name: "Submit" }))
    expect(onAnswer).toHaveBeenCalledTimes(1)
    expect(screen.getByRole("button", { name: "Submit" })).toBeDisabled()
    expect(screen.getByRole("button", { name: "Skip" })).toBeDisabled()
    expect(screen.getByRole("radio", { name: /Incremental/ })).toBeDisabled()
    expect(container.querySelector(".animate-spin")).not.toBeNull()
    d.resolve()
  })

  it("ignores a second submit while one is already in flight", () => {
    const d = deferred()
    const onAnswer = vi.fn(() => d.promise)
    renderWith(single, onAnswer)
    fireEvent.click(screen.getByRole("radio", { name: /Incremental/ }))
    const submit = screen.getByRole("button", { name: "Submit" })
    fireEvent.click(submit)
    fireEvent.click(submit)
    expect(onAnswer).toHaveBeenCalledTimes(1)
    d.resolve()
  })

  it("surfaces a retryable error and re-enables controls when answering fails", async () => {
    // A rejecting onAnswer stands in for both a backend failure and the
    // "no connection" path (the context now throws there instead of silently
    // resolving, which would otherwise strand the card in its in-flight state).
    const onAnswer = vi
      .fn()
      .mockRejectedValueOnce(new Error("boom"))
      .mockResolvedValueOnce(undefined)
    renderWith(single, onAnswer)
    fireEvent.click(screen.getByRole("radio", { name: /Rewrite/ }))
    fireEvent.click(screen.getByRole("button", { name: "Submit" }))
    // The failure surfaces inline and every control re-enables for a retry.
    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Couldn't submit. Please try again.")
    const submit = screen.getByRole("button", { name: "Submit" })
    expect(submit).not.toBeDisabled()
    expect(screen.getByRole("button", { name: "Skip" })).not.toBeDisabled()
    fireEvent.click(submit)
    expect(onAnswer).toHaveBeenCalledTimes(2)
  })

  it('renders a bare "(Recommended)" label literally instead of going empty', () => {
    const onlyRecommended: PendingQuestionState = {
      question_id: "q-3",
      created_at: "2026-01-01T00:00:00Z",
      questions: [
        {
          id: "qc",
          question: "Pick one",
          header: "Pick",
          multi_select: false,
          options: [
            { label: "(Recommended)", description: "" },
            { label: "Other path", description: "" },
          ],
        },
      ],
    }
    const onAnswer = renderCard(onlyRecommended)
    // The literal label is shown (not stripped to empty); selecting it submits
    // the verbatim label.
    fireEvent.click(screen.getByRole("radio", { name: "(Recommended)" }))
    fireEvent.click(screen.getByRole("button", { name: "Submit" }))
    expect(onAnswer).toHaveBeenCalledWith("q-3", {
      answers: [{ questionId: "qc", labels: ["(Recommended)"] }],
      declined: false,
    })
  })

  it("treats a real option labeled like the Other sentinel as a normal choice", () => {
    // The single-select RadioGroup uses index-based values, so an option whose
    // label happens to equal the internal "Other" sentinel still selects as a
    // real choice (no free-text input) and submits verbatim.
    const sentinel: PendingQuestionState = {
      question_id: "q-sentinel",
      created_at: "2026-01-01T00:00:00Z",
      questions: [
        {
          id: "qs",
          question: "Pick one",
          header: "Pick",
          multi_select: false,
          options: [
            { label: "__other__", description: "a real option" },
            { label: "Normal", description: "" },
          ],
        },
      ],
    }
    const onAnswer = renderCard(sentinel)
    fireEvent.click(screen.getByRole("radio", { name: /__other__/ }))
    // The free-text path is NOT engaged: no Other input appears.
    expect(
      screen.queryByPlaceholderText("Type your answer…")
    ).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole("button", { name: "Submit" }))
    expect(onAnswer).toHaveBeenCalledWith("q-sentinel", {
      answers: [{ questionId: "qs", labels: ["__other__"] }],
      declined: false,
    })
  })

  it("clears the in-flight guard so a reused instance can answer the next question", async () => {
    // After a successful submit the card normally unmounts; but if the same
    // instance is reused in place for a new question_id, the re-entrancy guard
    // must not stay latched (otherwise the next Submit silently no-ops).
    const dA = deferred()
    const onAnswer = vi
      .fn()
      .mockReturnValueOnce(dA.promise)
      .mockResolvedValue(undefined)
    const { rerender } = render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <AskQuestionCard question={single} onAnswer={onAnswer} />
      </NextIntlClientProvider>
    )
    fireEvent.click(screen.getByRole("radio", { name: /Incremental/ }))
    fireEvent.click(screen.getByRole("button", { name: "Submit" }))
    expect(onAnswer).toHaveBeenCalledTimes(1)
    // Resolve A; `run` clears the guard on the success path.
    dA.resolve()
    await dA.promise
    // The same instance is reused for a different question set (new question_id).
    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <AskQuestionCard question={multi} onAnswer={onAnswer} />
      </NextIntlClientProvider>
    )
    fireEvent.click(screen.getByRole("checkbox", { name: "auth" }))
    fireEvent.click(screen.getByRole("button", { name: "Submit" }))
    expect(onAnswer).toHaveBeenCalledTimes(2)
    expect(onAnswer).toHaveBeenLastCalledWith("q-2", {
      answers: [{ questionId: "qb", labels: ["auth"] }],
      declined: false,
    })
  })

  it("renders one tab per question when there are multiple", () => {
    renderCard(twoSingle)
    expect(screen.getAllByRole("tab")).toHaveLength(2)
  })

  it("auto-advances to the next tab after a single-select pick", () => {
    renderCard(twoSingle)
    expect(screen.getAllByRole("tab")[0]).toHaveAttribute(
      "aria-selected",
      "true"
    )
    // Picking an option on the first tab moves to the second.
    fireEvent.click(screen.getByRole("radio", { name: "X" }))
    const tabs = screen.getAllByRole("tab")
    expect(tabs[1]).toHaveAttribute("aria-selected", "true")
    expect(tabs[0]).toHaveAttribute("aria-selected", "false")
    // The second tab's options are now the visible ones.
    expect(screen.getByText("P")).toBeInTheDocument()
  })

  it("does not auto-advance on a multi-select pick", () => {
    renderCard(twoMultiFirst)
    fireEvent.click(screen.getByRole("checkbox", { name: "X" }))
    // Still on the first tab so further options can be picked.
    expect(screen.getAllByRole("tab")[0]).toHaveAttribute(
      "aria-selected",
      "true"
    )
    expect(screen.getByText("Y")).toBeInTheDocument()
  })

  it("marks a tab as confirmed once it is answered", () => {
    renderCard(twoSingle)
    expect(screen.getAllByRole("tab")[0]).toHaveAttribute(
      "data-answered",
      "false"
    )
    fireEvent.click(screen.getByRole("radio", { name: "X" }))
    expect(screen.getAllByRole("tab")[0]).toHaveAttribute(
      "data-answered",
      "true"
    )
  })

  it("advances the active tab with the Next button", () => {
    renderCard(twoSingle)
    expect(screen.getAllByRole("tab")[0]).toHaveAttribute(
      "aria-selected",
      "true"
    )
    fireEvent.click(screen.getByRole("button", { name: /Next/ }))
    const tabs = screen.getAllByRole("tab")
    expect(tabs[1]).toHaveAttribute("aria-selected", "true")
    // The last tab has no further tab, so Next is gone.
    expect(
      screen.queryByRole("button", { name: /Next/ })
    ).not.toBeInTheDocument()
  })

  it("shows a progress counter that advances as questions are answered", () => {
    renderCard(twoSingle)
    expect(screen.getByText("0/2")).toBeInTheDocument()
    fireEvent.click(screen.getByRole("radio", { name: "X" })) // auto-advances
    expect(screen.getByText("1/2")).toBeInTheDocument()
    fireEvent.click(screen.getByRole("radio", { name: "P" }))
    expect(screen.getByText("2/2")).toBeInTheDocument()
  })

  it("enables Submit only after every tab is answered, then submits all", () => {
    const onAnswer = renderCard(twoSingle)
    const submit = screen.getByRole("button", { name: /Submit/ })
    expect(submit).toBeDisabled()
    // Answer tab 1 (auto-advances to tab 2), then answer tab 2.
    fireEvent.click(screen.getByRole("radio", { name: "X" }))
    expect(screen.getByRole("button", { name: /Submit/ })).toBeDisabled()
    fireEvent.click(screen.getByRole("radio", { name: "P" }))
    const enabled = screen.getByRole("button", { name: /Submit/ })
    expect(enabled).not.toBeDisabled()
    fireEvent.click(enabled)
    expect(onAnswer).toHaveBeenCalledWith("q-two", {
      answers: [
        { questionId: "qa", labels: ["X"] },
        { questionId: "qb", labels: ["P"] },
      ],
      declined: false,
    })
  })

  it("resets selections when the question set is replaced in place", () => {
    // The shell renders the card without a per-question React key, so the card
    // must reset its own state when the question set changes underneath it.
    const onAnswer = vi.fn()
    const { rerender } = render(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <AskQuestionCard question={single} onAnswer={onAnswer} />
      </NextIntlClientProvider>
    )
    fireEvent.click(screen.getByRole("radio", { name: /Incremental/ }))
    expect(screen.getByRole("button", { name: "Submit" })).not.toBeDisabled()
    // Swap in a different question set (new question_id) at the same position.
    rerender(
      <NextIntlClientProvider locale="en" messages={enMessages}>
        <AskQuestionCard question={twoSingle} onAnswer={onAnswer} />
      </NextIntlClientProvider>
    )
    // No stale selection carries over: the new set renders fresh and ungated.
    expect(screen.queryByText("Incremental")).not.toBeInTheDocument()
    expect(screen.getAllByRole("tab")).toHaveLength(2)
    expect(screen.getByRole("button", { name: /Submit/ })).toBeDisabled()
  })

  it("renders nothing for an empty question set", () => {
    // Defensive guard: an empty set must not render a 0/0 card whose enabled
    // Submit would post an empty affirmative answer instead of a decline.
    const { container } = renderWith({ ...single, questions: [] }, vi.fn())
    expect(container).toBeEmptyDOMElement()
  })
})
