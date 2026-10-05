"use client"

import { useTranslations } from "next-intl"

export function RoundtableDetail({
  order,
  partial,
  waiting,
  showSynthesis,
  onSynthesis,
}: {
  order: string[]
  partial?: string
  waiting?: boolean
  showSynthesis: boolean
  onSynthesis?: () => void
}) {
  const t = useTranslations("Roundtable")
  return (
    <section>
      <ol>
        {order.map((member) => (
          <li key={member}>{member}</li>
        ))}
      </ol>
      {partial ? <p>{partial}</p> : null}
      {waiting ? <p>{t("waiting")}</p> : null}
      {showSynthesis ? (
        <button type="button" onClick={onSynthesis} disabled={!onSynthesis}>
          {t("synthesis")}
        </button>
      ) : null}
    </section>
  )
}
