"use client"

import { useEffect, useRef } from "react"
import { useTranslations } from "next-intl"

import {
  RichComposer,
  type RichComposerHandle,
} from "@/components/chat/composer/rich-composer"
import { useComposerMentionLabels } from "@/components/chat/composer/use-composer-mention-labels"
import { useReferenceSearchController } from "@/components/chat/composer/use-reference-search"
import type { ReferenceGroupKind } from "@/components/chat/composer/reference-search-controller"
import { roundtableTopicFromDoc } from "@/lib/roundtable/topic"
import { useDelegationProfileBootstrap } from "@/stores/delegation-profile-store"

const FILE_ONLY: readonly ReferenceGroupKind[] = ["file"]

/**
 * The roundtable question. Typing `@` opens the workspace file picker of the
 * chat composer, limited to files and folders; a pick is stored as plain
 * `@relative/path`, which members read inside the read-only `/workspace-ro`
 * mount. Controlled from the outside by `value`: an external change (a
 * loaded draft) replaces the document, the editor's own edits do not.
 */
export function RoundtableTopicInput({
  value,
  onChange,
  onBlur,
  workspaceId,
  workspacePath,
  invalid,
  describedBy,
}: {
  value: string
  onChange: (value: string) => void
  onBlur?: () => void
  workspaceId?: string
  workspacePath?: string
  invalid?: boolean
  describedBy?: string
}) {
  const t = useTranslations("Roundtable")
  useDelegationProfileBootstrap()
  const folderId = Number(workspaceId)
  const { groupLabels, uiLabels } = useComposerMentionLabels()
  const referenceController = useReferenceSearchController({
    folderId: Number.isInteger(folderId) && folderId > 0 ? folderId : null,
    defaultPath: workspacePath ?? null,
    enabled: !!workspacePath,
    labels: groupLabels,
  })
  const editorRef = useRef<RichComposerHandle>(null)
  const boxRef = useRef<HTMLDivElement>(null)
  const emitted = useRef(value)

  useEffect(() => {
    if (value === emitted.current) return
    emitted.current = value
    editorRef.current?.setText(value)
  }, [value])

  return (
    <div
      ref={boxRef}
      data-invalid={invalid || undefined}
      aria-describedby={describedBy}
      className="relative min-h-28 rounded-xl border border-input bg-background transition-colors focus-within:border-ring focus-within:ring-[3px] focus-within:ring-ring/50 data-[invalid]:border-destructive"
    >
      <RichComposer
        ref={editorRef}
        defaultText={value}
        placeholder={t("topicPlaceholder")}
        ariaLabel={t("topic")}
        referenceController={referenceController}
        mentionUiLabels={uiLabels}
        tabLabels={groupLabels}
        mentionKinds={FILE_ONLY}
        mentionAnchorRef={boxRef}
        onBlur={onBlur}
        onChange={() => {
          const json = editorRef.current?.getJSON()
          const next = roundtableTopicFromDoc(json)
          emitted.current = next
          onChange(next)
        }}
        className="max-h-[18rem] min-h-28 text-[15px] leading-relaxed md:text-[15px]"
      />
    </div>
  )
}
