"use client"

import { memo } from "react"
import { MessageResponse } from "@/components/ai-elements/message"
import { CodeBlockContainer } from "@/components/ai-elements/code-block"
import {
  joinStreamingMarkdown,
  type IncrementalStreamBlocks,
  type SealedMarkdownBlock,
} from "@/lib/markdown/incremental-stream-blocks"
import { streamingPerfRecorder } from "@/lib/perf/streaming-perf-recorder"

interface Props {
  document: IncrementalStreamBlocks
  onBlockRender?: (blockId: string) => void
  /**
   * `sealed-streaming` while live (no Mermaid); `complete` after handoff so
   * history can upgrade math/Mermaid/code engines.
   */
  richContentState?: "sealed-streaming" | "complete"
}

const SealedBlock = memo(
  function SealedBlock({
    block,
    onRender,
    richContentState,
  }: {
    block: SealedMarkdownBlock
    onRender?: (blockId: string) => void
    richContentState: "sealed-streaming" | "complete"
  }) {
    streamingPerfRecorder.countRender("markdownBlock")
    onRender?.(block.id)
    return (
      <MessageResponse mode="static" richContentState={richContentState}>
        {block.markdown}
      </MessageResponse>
    )
  },
  (previous, next) =>
    previous.block.id === next.block.id &&
    previous.block.markdown === next.block.markdown &&
    previous.onRender === next.onRender &&
    previous.richContentState === next.richContentState
)

function getOpenFenceTail(document: IncrementalStreamBlocks): {
  language: string
  prefix: string
  code: string
} | null {
  const fence = document.scanner.fence
  if (!fence) return null
  return {
    language: fence.language || "text",
    prefix: document.tail.slice(0, fence.openingOffset),
    code: document.tail.slice(fence.bodyOffset),
  }
}

export function StreamingMarkdownDocument({
  document,
  onBlockRender,
  richContentState = "sealed-streaming",
}: Props) {
  if (!document.valid) {
    return (
      <MessageResponse
        mode={richContentState === "complete" ? "static" : "streaming"}
        richContentState={
          richContentState === "complete" ? "complete" : undefined
        }
      >
        {joinStreamingMarkdown(document)}
      </MessageResponse>
    )
  }
  const openFence = getOpenFenceTail(document)
  return (
    <div className="space-y-4">
      {document.sealed.map((block) => (
        <SealedBlock
          key={block.id}
          block={block}
          onRender={onBlockRender}
          richContentState={richContentState}
        />
      ))}
      {openFence ? (
        <>
          {openFence.prefix ? (
            <div className="whitespace-pre-wrap break-words text-sm select-text">
              {openFence.prefix}
            </div>
          ) : null}
          <CodeBlockContainer language={openFence.language}>
            <pre
              data-testid="streaming-code-tail"
              className="m-0 whitespace-pre-wrap break-words p-4 font-mono text-sm select-text"
            >
              <code>{openFence.code}</code>
            </pre>
          </CodeBlockContainer>
        </>
      ) : document.tail ? (
        <div
          data-testid="streaming-markdown-tail"
          className="whitespace-pre-wrap break-words text-sm select-text"
        >
          {document.tail}
        </div>
      ) : null}
    </div>
  )
}
