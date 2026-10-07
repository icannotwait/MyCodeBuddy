import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import test from "node:test"
import { fileURLToPath } from "node:url"

const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..")

// Compile the actual desktop entry point on stable without pulling in Tauri's
// platform libraries. Only the library boundary is a test double; main.rs is
// used unchanged, so unstable APIs and incorrect early exits still fail here.
test("desktop qualification preserves exit codes", async (t) => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), "codeg-entrypoint-"))
  const library = join(fixtureRoot, "libcodeg_lib.rlib")
  const binary = join(
    fixtureRoot,
    process.platform === "win32" ? "codeg-fixture.exe" : "codeg-fixture"
  )
  const stub = join(fixtureRoot, "lib.rs")

  function rustc(args) {
    const result = spawnSync("rustc", ["--edition=2021", ...args], {
      encoding: "utf8",
      maxBuffer: 10 * 1024 * 1024,
    })
    assert.ifError(result.error)
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`)
  }

  try {
    writeFileSync(
      stub,
      `pub mod roundtable {
    pub fn roundtable_qualify_requested(args: &[String]) -> bool {
        args.iter().any(|arg| arg == "roundtable-qualify")
    }
    pub fn run_roundtable_qualify() -> std::process::ExitCode {
        println!("qualification");
        std::process::ExitCode::from(std::env::var("FIXTURE_EXIT_CODE").unwrap().parse::<u8>().unwrap())
    }
}
pub mod logging {
    pub mod init {
        pub fn init_stderr_only() {}
    }
}
pub mod git_credential {
    pub fn run_credential_helper() {
        println!("credential-helper");
    }
}
pub fn run() {
    println!("desktop-runtime");
}
`
    )
    rustc([
      "--crate-name",
      "codeg_lib",
      "--crate-type=rlib",
      stub,
      "-o",
      library,
    ])
    rustc([
      join(repositoryRoot, "src-tauri", "src", "main.rs"),
      "--extern",
      `codeg_lib=${library}`,
      "-o",
      binary,
    ])

    for (const exitCode of [0, 1, 2]) {
      await t.test(`qualification returns ${exitCode}`, () => {
        const result = spawnSync(binary, ["roundtable-qualify"], {
          encoding: "utf8",
          env: { ...process.env, FIXTURE_EXIT_CODE: String(exitCode) },
        })
        assert.ifError(result.error)
        assert.equal(result.status, exitCode, result.stderr)
        assert.equal(result.stdout.trim(), "qualification")
      })
    }

    for (const [args, marker] of [
      [["--credential-helper"], "credential-helper"],
      [[], "desktop-runtime"],
    ]) {
      await t.test(`${marker} returns success without another mode`, () => {
        const result = spawnSync(binary, args, { encoding: "utf8" })
        assert.ifError(result.error)
        assert.equal(result.status, 0, result.stderr)
        assert.equal(result.stdout.trim(), marker)
      })
    }
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})
