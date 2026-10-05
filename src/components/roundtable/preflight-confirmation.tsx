"use client"

export function PreflightConfirmation({
  targets,
  tools,
  network,
  writes,
  budget,
}: {
  targets: string[]
  tools: string[]
  network: string
  writes: string
  budget: string
}) {
  return (
    <dl>
      <dt>目标</dt>
      <dd>{targets.join("、")}</dd>
      <dt>工具</dt>
      <dd>{tools.join("、")}</dd>
      <dt>网络</dt>
      <dd>{network}</dd>
      <dt>写入</dt>
      <dd>{writes}</dd>
      <dt>预算</dt>
      <dd>{budget}</dd>
    </dl>
  )
}
