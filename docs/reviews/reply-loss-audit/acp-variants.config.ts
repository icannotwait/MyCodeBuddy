import base from "../../../vitest.config"

// Exercise existing integration fixtures with different delivery order, without
// editing the fixtures at runtime. Failed at baseline; pass after remediation.
const target = "src/contexts/acp-connections-context.test.tsx"
const boundaryTest =
  "promotes two previously unprocessed turn boundaries in one frame"
const gapTest =
  "lands coalesced deltas before a mid-turn snapshot replaces the message"
const completedGapTest =
  "audit: completed gap snapshot preserves accepted queued text"

function changeTest(
  code: string,
  name: string,
  update: (test: string) => string
) {
  const start = code.indexOf(`  it("${name}"`)
  if (start < 0) throw new Error(`Audit fixture changed: ${name}`)
  const next = code.indexOf("\n  it(", start + 1)
  const end = next < 0 ? code.length : next
  const original = code.slice(start, end)
  const changed = update(original)
  if (changed === original)
    throw new Error(`Audit variant not applied: ${name}`)
  return code.slice(0, start) + changed + code.slice(end)
}

export default {
  ...base,
  plugins: [
    ...(base.plugins ?? []),
    {
      name: "reply-loss-audit-delivery-order",
      enforce: "pre" as const,
      transform(code: string, id: string) {
        if (!id.replaceAll("\\", "/").endsWith(target)) return
        let changed = changeTest(code, boundaryTest, (test) =>
          test.replace(
            'content("owner-conn", 2, "answer A"),',
            `content("owner-conn", 2, "answer A"),
          ])
        )
        h.runAnimationFrame()
      })
      act(() => {
        h.emitDesktopBatch(
          batch(2, [`
          )
        )
        changed = changeTest(changed, gapTest, (test) => {
          const recovered = test.replace(
            /hydrateSnapshot\(handlers, \{\s*event_seq: 9,\s*\} as unknown as LiveSessionSnapshot\)/,
            `h.acpGetSessionSnapshot.mockResolvedValue({ event_seq: 9 })
      await act(async () => {
        emitAcpEvent(handlers, {
          seq: 4,
          connection_id: "spawned-conn",
          type: "content_delta",
          text: "later",
        })
        await Promise.resolve()
        await Promise.resolve()
      })`
          )
          const completed = recovered
            .replace(gapTest, completedGapTest)
            .replace(
              "const handlers = await mountStreamingOwner()",
              `const handlers = await mountStreamingOwner()
    const published: string[] = []
    h.actions!.registerLiveMessageSink(TAB, (message) => {
      published.push(message?.content.flatMap(block =>
        block.type === "text" ? [block.text] : []).join("") ?? "")
    })`
            )
            .replace('status: "prompting",', 'status: "connected",')
            .replace(
              /liveMessage: \{[\s\S]*?startedAt: 0,\s*\},/,
              "liveMessage: null,"
            )
            .replace(
              'expect(liveText()).toBe("hello ")',
              'expect(published.join("")).toContain("hello ")'
            )
          return recovered + "\n" + completed
        })
        return { code: changed, map: null }
      },
    },
  ],
  test: {
    ...base.test,
    include: [target],
    testNamePattern: `${boundaryTest}|${gapTest}|${completedGapTest}`,
  },
}
