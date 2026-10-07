"use client"

import { useTranslations } from "next-intl"
import type {
  RoundtableRecipient,
  RoundtableSourceEntry,
  RoundtableSourceManifest,
} from "@/lib/roundtable/types"

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
}) {
  const t = useTranslations("Roundtable")
  return (
    <dl className="space-y-2 break-words">
      <dt>{t("targets")}</dt>
      <dd>
        <ul>
          {recipients.map((recipient) => (
            <li key={recipient.ordinal}>
              {t("member")} {recipient.ordinal + 1}
              {recipient.ordinal === moderatorOrdinal
                ? ` / ${t("moderator")}`
                : ""}
              {" · "}
              {recipient.provider_ref}
              {" · "}
              {recipient.origin}
              {" · "}
              {t("model")}: {recipient.model}
              {" · "}
              {t("agent")}: {recipient.agent}
              {" · "}
              {t("effort")}: {recipient.effort ?? t("defaultEffort")}
            </li>
          ))}
        </ul>
        {recipients.length === 0 ? t("unresolvedRecipients") : null}
      </dd>
      <dt>{t("frozenSources")}</dt>
      <dd>
        {sourceManifests.length === 0 && selectedPaths.length === 0 ? (
          <p>{t("noSources")}</p>
        ) : null}
        {selectedPaths.length > 0 ? (
          <>
            <p>{t("captureOnCreate")}</p>
            <ul>
              {selectedPaths.map((path, index) => (
                <li key={`${index}:${path}`}>{path}</li>
              ))}
            </ul>
          </>
        ) : null}
        {sourceManifests.map(({ manifest, hash }) => (
          <div key={manifest.manifest_id}>
            <p>
              {t("snapshotHash")}: {hash}
            </p>
            <ul>
              {manifest.entries.map((entry) => (
                <li key={entry.path}>
                  <span>
                    {entry.path} · {entry.size} {t("bytes")} ·{" "}
                    {entry.content_hash}
                  </span>
                  {entry.text_admissible && onPreview ? (
                    <button
                      type="button"
                      className="ml-2 underline"
                      onClick={() => onPreview(entry)}
                    >
                      {t("previewSource")}: {entry.path}
                    </button>
                  ) : null}
                  {!entry.text_admissible ? <p>{t("binarySource")}</p> : null}
                  {sourcePreviews[entry.content_hash] !== undefined ? (
                    <pre className="whitespace-pre-wrap break-words">
                      {sourcePreviews[entry.content_hash]}
                    </pre>
                  ) : null}
                </li>
              ))}
            </ul>
          </div>
        ))}
      </dd>
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
