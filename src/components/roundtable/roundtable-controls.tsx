"use client"

export function RoundtableControls({
  moderatorFailed,
}: {
  moderatorFailed: boolean
}) {
  return (
    <section>
      <button type="button">立即暂停，保留已完成结果</button>
      {moderatorFailed ? <button type="button">只重试主持</button> : null}
    </section>
  )
}
