import { describe, expect, it } from "vitest"
import {
  ComparisonRecorder,
  LONG_FRAME_MS,
  ManualRafScheduler,
  recorderFromFrames,
} from "./eui-comparison-recorder"

describe("eui-comparison-recorder", () => {
  it("uses first presentation and the fixed 50 ms threshold", () => {
    const run = recorderFromFrames([0, 16, 32, 92, 108], {
      t0: 0,
      firstToken: 8,
      firstPresented: 16,
      end: 108,
    }).finish()
    expect(run.firstPresentedLatencyMs).toBe(16)
    expect(run.frameIntervalP95Ms).toBe(60)
    expect(run.longFrameThresholdMs).toBe(50)
    expect(run.longFrameCount).toBe(1)
    expect(LONG_FRAME_MS).toBe(50)
  })

  it("marks only in the second RAF after assistant DOM commit", () => {
    const raf = new ManualRafScheduler()
    const recorder = new ComparisonRecorder(
      {
        shell: "webview",
        agent: "codex",
        promptId: "continuous-text-v1",
        buildType: "release",
        backend: "tauri-webview",
        gitCommit: "066ce16401cbd5de0822f5f721806f6624f1eade",
        notes: "local comparison capture",
      },
      raf.request,
    )
    recorder.assistantCommitted("first token")
    expect(recorder.firstPresentedNs).toBeUndefined()
    raf.flushOne(10) // RAF1, before eligible paint
    expect(recorder.firstPresentedNs).toBeUndefined()
    raf.markPaint()
    raf.flushOne(16) // RAF2, after eligible paint
    expect(recorder.firstPresentedNs).toBe(16)
  })
})
