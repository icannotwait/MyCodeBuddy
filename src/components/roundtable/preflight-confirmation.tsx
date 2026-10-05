"use client"

import { useTranslations } from "next-intl"

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
  const t = useTranslations("Roundtable")
  return (
    <dl>
      <dt>{t("targets")}</dt>
      <dd>{targets.join("、")}</dd>
      <dt>{t("tools")}</dt>
      <dd>{tools.join("、")}</dd>
      <dt>{t("network")}</dt>
      <dd>{network}</dd>
      <dt>{t("writes")}</dt>
      <dd>{writes}</dd>
      <dt>{t("budget")}</dt>
      <dd>{budget}</dd>
    </dl>
  )
}
