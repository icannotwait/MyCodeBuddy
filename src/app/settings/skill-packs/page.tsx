"use client"

import { Suspense } from "react"
import { useTranslations } from "next-intl"
import { SkillPacksSettings } from "@/components/settings/skill-packs-settings"

export default function SettingsSkillPacksPage() {
  const t = useTranslations("SettingsPages")

  return (
    <Suspense
      fallback={
        <div className="h-full flex items-center justify-center text-sm text-muted-foreground">
          {t("skillPacksLoading")}
        </div>
      }
    >
      <SkillPacksSettings />
    </Suspense>
  )
}
