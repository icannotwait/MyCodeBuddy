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

test("Rust CI serializes desktop, server, and small macOS builds without dropping targets", () => {
  const rust = workflow.jobs.rust
  const matrix = rust.strategy.matrix
  assert.deepEqual(matrix.os, [
    "ubuntu-22.04",
    "macos-latest",
    "windows-latest",
  ])
  assert.deepEqual(matrix.mode, ["desktop", "server", "cli"])

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
      // Server lib tests link one large binary. One rustc job keeps the
      // 7 GiB Ubuntu runner from dropping the hosted connection.
      const expected =
        mode === "desktop" || mode === "server" || os === "macos-latest" ? 1 : 4
      assert.equal(settings.build_jobs ?? 4, expected, `${os} / ${mode}`)
    }
  }
  assert.equal(rust.env.CARGO_BUILD_JOBS, "${{ matrix.build_jobs || 4 }}")
})
