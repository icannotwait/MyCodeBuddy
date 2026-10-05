"use client"

export function CreateRoundtable({
  mode,
  quotaError,
  confirmed,
  onConfirm,
}: {
  mode: "parallel" | "serial"
  quotaError?: string
  confirmed: boolean
  onConfirm: () => void
}) {
  return (
    <section>
      <h1>开始</h1>
      <p>{mode === "parallel" ? "并行咨询" : "串行咨询"}</p>
      {quotaError ? <p role="alert">{quotaError}</p> : null}
      <button type="button" onClick={onConfirm}>
        确认预检
      </button>
      <button type="button" disabled={!confirmed}>
        开始
      </button>
    </section>
  )
}
