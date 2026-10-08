import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import test from "node:test"
import { parse } from "yaml"

const workflow = parse(
  readFileSync(
    new URL("../.github/workflows/test.yml", import.meta.url),
    "utf8"
  )
)

test("Rust CI serializes desktop and small macOS builds without dropping targets", () => {
  const rust = workflow.jobs.rust
  const matrix = rust.strategy.matrix
  assert.deepEqual(matrix.os, [
    "ubuntu-22.04",
    "macos-latest",
    "windows-latest",
  ])
  assert.deepEqual(matrix.mode, ["desktop", "server"])

  for (const os of matrix.os) {
    for (const mode of matrix.mode) {
      const settings = Object.assign(
        {},
        ...matrix.include.filter(
          (entry) =>
            (entry.os === undefined || entry.os === os) &&
            (entry.mode === undefined || entry.mode === mode)
        )
      )
      const expected = mode === "desktop" || os === "macos-latest" ? 1 : 4
      assert.equal(settings.build_jobs ?? 4, expected, `${os} / ${mode}`)
    }
  }
  assert.equal(rust.env.CARGO_BUILD_JOBS, "${{ matrix.build_jobs || 4 }}")
})
