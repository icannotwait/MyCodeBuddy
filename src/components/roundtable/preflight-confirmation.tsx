"use client"

import type { ReactNode } from "react"
import { useTranslations } from "next-intl"
import { Badge } from "@/components/ui/badge"
import { brandStyle, type SeatBrand } from "@/lib/roundtable/brand"
import type {
  RoundtableRecipient,
  RoundtableSourceEntry,
  RoundtableSourceManifest,
} from "@/lib/roundtable/types"
import { cn } from "@/lib/utils"
import { BRAND_CLASSES, RoundtableSpeakerAvatar } from "./roundtable-transcript"

function Group({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0 space-y-2">
      <dt className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
        {label}
      </dt>
      <dd className="min-w-0">{children}</dd>
    </div>
  )
}

function Fact({
  label,
  children,
  className,
}: {
  label: string
  children: ReactNode
  className?: string
}) {
  return (
    <div
      className={cn("min-w-0 rounded-lg border bg-background p-3", className)}
    >
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="mt-1 min-w-0 text-sm font-medium [overflow-wrap:anywhere]">
        {children}
      </dd>
    </div>
  )
}

export function PreflightConfirmation({
  recipients,
  moderatorOrdinal,
  sourceManifests,
  selectedPaths,
  onPreview,
  sourcePreviews = {},
  tools,
  network,
  writes,
  budget,
  seatBrands,
}: {
  recipients: RoundtableRecipient[]
  moderatorOrdinal: number
  sourceManifests: RoundtableSourceManifest[]
  selectedPaths: string[]
  onPreview?: (entry: RoundtableSourceEntry) => void
  sourcePreviews?: Record<string, string>
  tools: string[]
  network: string
  writes: string
  budget: string
  /** Seat brands; when given, recipients show the same avatar as the room. */
  seatBrands?: Map<number, SeatBrand>
}) {
  const t = useTranslations("Roundtable")
  return (
    <div className="flex min-w-0 flex-col gap-4 [overflow-wrap:anywhere]">
      <dl className="flex min-w-0 flex-col gap-4 [overflow-wrap:anywhere]">
        <Group label={t("targets")}>
          {recipients.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              {t("unresolvedRecipients")}
            </p>
          ) : (
            <ul className="grid min-w-0 gap-2 lg:grid-cols-2">
              {recipients.map((recipient) => {
                const seat = seatBrands?.get(recipient.ordinal)
                return (
                  <li
                    key={recipient.ordinal}
                    data-testid="preflight-recipient"
                    style={seat ? brandStyle(seat.brand) : undefined}
                    className={cn(
                      "flex min-w-0 items-start gap-3 rounded-lg border bg-background p-3",
                      seat && BRAND_CLASSES.border
                    )}
                  >
                    {seat ? (
                      <RoundtableSpeakerAvatar
                        agent={recipient.agent}
                        seat={seat}
                        seatNumber={recipient.ordinal + 1}
                      />
                    ) : null}
                    <div className="min-w-0 flex-1 space-y-1 text-sm">
                      <p className="flex flex-wrap items-center gap-x-2 gap-y-1 font-medium">
                        <span>
                          {t("member")} {recipient.ordinal + 1}
                        </span>
                        {recipient.ordinal === moderatorOrdinal ? (
                          <Badge variant="secondary">{t("moderator")}</Badge>
                        ) : null}
                      </p>
                      <p className="min-w-0 text-muted-foreground">
                        {t("model")}:{" "}
                        <span className="font-mono text-xs text-foreground">
                          {recipient.model}
                        </span>
                      </p>
                      <p className="flex min-w-0 flex-wrap gap-x-3 gap-y-0.5 text-xs text-muted-foreground">
                        <span>
                          {t("agent")}: <span>{recipient.agent}</span>
                        </span>
                        <span>
                          {t("effort")}:{" "}
                          <span>{recipient.effort ?? t("defaultEffort")}</span>
                        </span>
                      </p>
                      <p className="flex min-w-0 flex-wrap gap-x-3 gap-y-0.5 font-mono text-xs text-muted-foreground">
                        <span>{recipient.provider_ref}</span>
                        <span>{recipient.origin}</span>
                      </p>
                    </div>
                  </li>
                )
              })}
            </ul>
          )}
        </Group>
        <Group label={t("frozenSources")}>
          {sourceManifests.length === 0 && selectedPaths.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t("noSources")}</p>
          ) : null}
          {selectedPaths.length > 0 ? (
            <div className="space-y-2 rounded-lg border bg-background p-3 text-sm">
              <p className="text-muted-foreground">{t("captureOnCreate")}</p>
              <ul className="space-y-1 font-mono text-xs">
                {selectedPaths.map((path, index) => (
                  <li key={`${index}:${path}`}>{path}</li>
                ))}
              </ul>
            </div>
          ) : null}
          {sourceManifests.map(({ manifest, hash }) => (
            <div
              key={manifest.manifest_id}
              className="space-y-2 rounded-lg border bg-background p-3 text-sm"
            >
              <p className="text-xs text-muted-foreground">
                {t("snapshotHash")}: <span className="font-mono">{hash}</span>
              </p>
              <ul className="space-y-2">
                {manifest.entries.map((entry) => (
                  <li key={entry.path} className="min-w-0 space-y-1">
                    <p className="flex min-w-0 flex-wrap gap-x-2 text-xs">
                      <span className="font-mono font-medium">
                        {entry.path}
                      </span>
                      <span className="text-muted-foreground">
                        {entry.size} {t("bytes")}
                      </span>
                      <span className="font-mono text-muted-foreground">
                        {entry.content_hash}
                      </span>
                    </p>
                    {entry.text_admissible && onPreview ? (
                      <button
                        type="button"
                        className="rounded-sm text-start text-xs underline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
                        onClick={() => onPreview(entry)}
                      >
                        {t("previewSource")}: {entry.path}
                      </button>
                    ) : null}
                    {!entry.text_admissible ? (
                      <p className="text-xs text-muted-foreground">
                        {t("binarySource")}
                      </p>
                    ) : null}
                    {sourcePreviews[entry.content_hash] !== undefined ? (
                      <pre className="max-h-64 min-w-0 overflow-auto rounded-md bg-muted/50 p-2 text-xs whitespace-pre-wrap [overflow-wrap:anywhere]">
                        {sourcePreviews[entry.content_hash]}
                      </pre>
                    ) : null}
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </Group>
      </dl>
      <dl className="grid min-w-0 grid-cols-2 gap-2 lg:grid-cols-4">
        <Fact label={t("tools")} className="col-span-2 lg:col-span-1">
          {tools.length > 0 ? (
            <span className="flex flex-wrap gap-1">
              {tools.map((tool) => (
                <code
                  key={tool}
                  className="rounded bg-muted px-1.5 py-0.5 text-xs font-normal"
                >
                  {tool}
                </code>
              ))}
            </span>
          ) : (
            "—"
          )}
        </Fact>
        <Fact label={t("network")}>
          <code className="text-xs">{network}</code>
        </Fact>
        <Fact label={t("writes")}>
          <code className="text-xs">{writes}</code>
        </Fact>
        <Fact label={t("budget")}>{budget}</Fact>
      </dl>
    </div>
  )
}
