"use client"

export function RoundtableDetail({
  order,
  partial,
  waiting,
  showSynthesis,
}: {
  order: string[]
  partial?: string
  waiting?: boolean
  showSynthesis: boolean
}) {
  return (
    <section>
      <ol>
        {order.map((member) => (
          <li key={member}>{member}</li>
        ))}
      </ol>
      {partial ? <p>{partial}</p> : null}
      {waiting ? <p>等待其余成员</p> : null}
      {showSynthesis ? <button type="button">综合</button> : null}
    </section>
  )
}
