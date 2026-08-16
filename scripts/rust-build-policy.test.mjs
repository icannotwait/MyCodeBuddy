import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import test from "node:test"
import { fileURLToPath } from "node:url"

const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..")

test("repository Cargo config omits debug and incremental artifacts", () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), "codeg-cargo-policy-"))
  const manifestPath = join(fixtureRoot, "Cargo.toml")
  const targetPath = join(fixtureRoot, "target")

  try {
    mkdirSync(join(fixtureRoot, "src"))
    writeFileSync(
      manifestPath,
      '[package]\nname = "cargo-policy-fixture"\nversion = "0.1.0"\nedition = "2021"\n'
    )
    writeFileSync(
      join(fixtureRoot, "src", "lib.rs"),
      "pub fn value() -> u8 { 1 }\n"
    )

    const result = spawnSync(
      "cargo",
      [
        "--config",
        join(repositoryRoot, ".cargo", "config.toml"),
        "test",
        "--no-run",
        "-v",
        "--manifest-path",
        manifestPath,
        "--target-dir",
        targetPath,
      ],
      {
        encoding: "utf8",
        env: { ...process.env, CARGO_TERM_COLOR: "never" },
        maxBuffer: 10 * 1024 * 1024,
      }
    )
    const output = `${result.stdout}\n${result.stderr}`

    assert.equal(result.status, 0, output)
    assert.match(output, /rustc.*cargo_policy_fixture/)
    assert.doesNotMatch(output, /-C debuginfo=[1-9]/)
    assert.doesNotMatch(output, /-C incremental=/)
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})
