import assert from "node:assert/strict"
import {
  chmodSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs"
import { tmpdir } from "node:os"
import { delimiter, join } from "node:path"
import { spawnSync } from "node:child_process"
import test from "node:test"
import {
  buildSigningPaths,
  replacePublicKeyInConfigText,
  updatePublicKey,
  validateGeneratedPublicKey,
} from "./prepare-updater-signing-key.mjs"

const validPublicKey =
  "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDA4QUIwRUY2OUVCRTczM0EKUldRNmM3NmU5ZzZyQ0NmdlEzNmc2czl1UnRVSnpQTGMzRW1SbXFtdWx4dVVyYU5DMGVPYzkxYk8K"

function makePublicKeyDocument(keyLine) {
  return Buffer.from(
    `untrusted comment: minisign public key: 0123456789ABCDEF\n${keyLine}\n`,
    "utf8"
  ).toString("base64")
}

test("validateGeneratedPublicKey accepts a canonical minisign public key", () => {
  assert.equal(validateGeneratedPublicKey(validPublicKey), validPublicKey)
})

test("validateGeneratedPublicKey rejects malformed public key documents", () => {
  const canonicalWrongLengthPayload = Buffer.concat([
    Buffer.from("Ed", "utf8"),
    Buffer.alloc(39),
  ]).toString("base64")
  const cases = [
    validPublicKey + "=",
    Buffer.from("untrusted comment: minisign public key\n", "utf8").toString(
      "base64"
    ),
    Buffer.from(
      `not a minisign public key\n${Buffer.from(validPublicKey, "base64").toString("utf8").split("\n")[1]}\n`,
      "utf8"
    ).toString("base64"),
    makePublicKeyDocument(
      `XX${Buffer.from(validPublicKey, "base64").toString("utf8").split("\n")[1].slice(2)}`
    ),
    makePublicKeyDocument(canonicalWrongLengthPayload),
    makePublicKeyDocument(
      `${Buffer.from(validPublicKey, "base64").toString("utf8").split("\n")[1]}=`
    ),
  ]

  for (const publicKey of cases) {
    assert.throws(
      () => validateGeneratedPublicKey(publicKey),
      /generated updater public key is invalid/
    )
  }
})

test("updatePublicKey changes only plugins.updater.pubkey", () => {
  const config = {
    productName: "MyCodeBuddy",
    bundle: {
      active: true,
    },
    plugins: {
      updater: {
        pubkey: "old-public-key",
        endpoints: ["https://example.com/latest.json"],
      },
      opener: {
        enabled: true,
      },
    },
  }

  const updated = updatePublicKey(config, "public-key")

  assert.deepEqual(updated, {
    productName: "MyCodeBuddy",
    bundle: {
      active: true,
    },
    plugins: {
      updater: {
        pubkey: "public-key",
        endpoints: ["https://example.com/latest.json"],
      },
      opener: {
        enabled: true,
      },
    },
  })
  assert.equal(config.plugins.updater.pubkey, "old-public-key")
})

test("replacePublicKeyInConfigText preserves all other config text", () => {
  const configText = `{
  "bundle": {
    "externalBin": ["binaries/codeg-mcp"]
  },
  "plugins": {
    "updater": {
      "pubkey": "old-public-key",
      "endpoints": ["https://example.com/latest.json"]
    }
  }
}
`

  assert.equal(
    replacePublicKeyInConfigText(configText, "public-key"),
    configText.replace('"old-public-key"', '"public-key"')
  )
})

test("buildSigningPaths keeps all generated files under the signing directory", () => {
  const signingDir = join("/home/test", ".config", "mycodebuddy", "signing")

  assert.deepEqual(buildSigningPaths("/home/test"), {
    signingDir,
    privateKey: join(signingDir, "updater-signing.key"),
    publicKey: join(signingDir, "updater-signing.key.pub"),
    passwordFile: join(signingDir, "updater-signing.password"),
    localEnv: join(signingDir, "local-build.env"),
    githubSecrets: join(signingDir, "GITHUB_SECRETS.md"),
  })
})

test("CLI safely generates, validates, and installs updater signing files", () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), "updater-signing-test-"))
  const fixtureScripts = join(fixtureRoot, "scripts")
  const fixtureTauri = join(fixtureRoot, "src-tauri")
  const fixtureHome = join(fixtureRoot, "home")
  const fixtureBin = join(fixtureRoot, "bin")
  const fixtureScript = join(fixtureScripts, "prepare-updater-signing-key.mjs")

  try {
    mkdirSync(fixtureScripts)
    mkdirSync(fixtureTauri)
    mkdirSync(fixtureHome)
    mkdirSync(fixtureBin)
    cpSync(
      join(import.meta.dirname, "prepare-updater-signing-key.mjs"),
      fixtureScript
    )
    writeFileSync(
      join(fixtureTauri, "tauri.conf.json"),
      `${JSON.stringify({
        plugins: {
          updater: {
            pubkey: "old-public-key",
            endpoints: ["https://example.com/latest.json"],
          },
        },
      })}\n`
    )

    const fakePnpm = join(fixtureBin, "pnpm")
    writeFileSync(
      fakePnpm,
      `#!/usr/bin/env node
import { writeFileSync } from "node:fs"
const args = process.argv.slice(2)
if (process.env.PNPM_CONFIG_REPORTER !== "silent") {
  process.stderr.write("$ " + args.join(" ") + "\\n")
}
const privateKey = args[args.indexOf("-w") + 1]
writeFileSync(privateKey, "test-private-key")
if (process.env.TEST_SIGNER_FAIL === "1") {
  process.exit(7)
}
writeFileSync(
  privateKey + ".pub",
  process.env.TEST_SIGNER_INVALID_PUBLIC_KEY === "1"
    ? "not-a-minisign-public-key"
    : ${JSON.stringify(validPublicKey)}
)
`
    )
    chmodSync(fakePnpm, 0o755)

    const signingDir = join(fixtureHome, ".config", "mycodebuddy", "signing")
    const privateKey = join(signingDir, "updater-signing.key")
    const passwordFile = join(signingDir, "updater-signing.password")
    const localEnv = join(signingDir, "local-build.env")
    const githubSecrets = join(signingDir, "GITHUB_SECRETS.md")
    const lockFile = join(signingDir, ".updater-signing-generation.lock")
    const baseEnv = {
      ...process.env,
      HOME: fixtureHome,
      PATH: `${fixtureBin}${delimiter}${process.env.PATH}`,
      PNPM_CONFIG_REPORTER: "",
    }

    mkdirSync(signingDir, { recursive: true })
    writeFileSync(lockFile, "active-generation")
    const lockedResult = spawnSync(
      process.execPath,
      [realpathSync(fixtureScript)],
      {
        encoding: "utf8",
        env: baseEnv,
      }
    )
    assert.equal(lockedResult.status, 1, "CLI must refuse a concurrent run")
    assert.equal(lockedResult.stdout, "")
    assert.equal(
      lockedResult.stderr,
      "updater signing generation is already in progress\n"
    )
    rmSync(lockFile)

    writeFileSync(passwordFile, "preserve-existing-password-file")
    const existingFileResult = spawnSync(
      process.execPath,
      [realpathSync(fixtureScript)],
      {
        encoding: "utf8",
        env: baseEnv,
      }
    )

    assert.equal(
      existingFileResult.status,
      1,
      "CLI must refuse any existing signing output"
    )
    assert.equal(
      readFileSync(passwordFile, "utf8"),
      "preserve-existing-password-file"
    )
    rmSync(signingDir, { recursive: true, force: true })

    const failedResult = spawnSync(
      process.execPath,
      [realpathSync(fixtureScript)],
      {
        encoding: "utf8",
        env: {
          ...baseEnv,
          TEST_SIGNER_FAIL: "1",
        },
      }
    )

    assert.equal(failedResult.status, 1, "fixture signer failure should fail")
    assert.ok(
      failedResult.stderr === "updater signing key generation failed\n",
      "signer failure output must be a fixed safe error"
    )
    assert.equal(statSync(privateKey).mode & 0o777, 0o600)
    assert.ok(existsSync(privateKey), "signer private key must be preserved")
    assert.ok(!existsSync(lockFile), "failed generation must release its lock")
    rmSync(signingDir, { recursive: true, force: true })

    const configPath = join(fixtureTauri, "tauri.conf.json")
    const configBeforeInvalidPublicKey = readFileSync(configPath, "utf8")
    const invalidPublicKeyResult = spawnSync(
      process.execPath,
      [realpathSync(fixtureScript)],
      {
        encoding: "utf8",
        env: {
          ...baseEnv,
          TEST_SIGNER_INVALID_PUBLIC_KEY: "1",
        },
      }
    )
    assert.equal(invalidPublicKeyResult.status, 1)
    assert.equal(
      invalidPublicKeyResult.stderr,
      "updater signing key generation failed\n"
    )
    assert.equal(readFileSync(configPath, "utf8"), configBeforeInvalidPublicKey)
    assert.equal(statSync(privateKey).mode & 0o777, 0o600)
    assert.ok(existsSync(join(signingDir, "updater-signing.key.pub")))
    assert.ok(!existsSync(passwordFile))
    assert.ok(!existsSync(lockFile), "invalid public key must release its lock")
    rmSync(signingDir, { recursive: true, force: true })

    const result = spawnSync(process.execPath, [realpathSync(fixtureScript)], {
      encoding: "utf8",
      env: baseEnv,
    })

    assert.equal(result.status, 0, "fixture CLI should complete successfully")
    assert.ok(existsSync(privateKey), "fixture CLI should invoke the signer")
    assert.equal(
      result.stderr.length,
      0,
      "signer child output must be fully suppressed"
    )
    const password = readFileSync(passwordFile, "utf8")
    assert.equal(Buffer.from(password, "base64url").length, 32)
    assert.match(password, /^[A-Za-z0-9_-]{43}$/)
    assert.ok(
      !result.stdout.includes(password),
      "successful CLI output must not include the password"
    )

    assert.equal(statSync(signingDir).mode & 0o777, 0o700)
    assert.equal(statSync(privateKey).mode & 0o777, 0o600)
    assert.equal(statSync(passwordFile).mode & 0o777, 0o600)
    assert.equal(statSync(localEnv).mode & 0o777, 0o600)
    assert.equal(statSync(githubSecrets).mode & 0o777, 0o600)

    const localEnvText = readFileSync(localEnv, "utf8")
    const localEnvLines = localEnvText.split("\n")
    assert.equal(localEnvText.endsWith("\n"), false)
    assert.equal(localEnvLines[0], `TAURI_SIGNING_PRIVATE_KEY='${privateKey}'`)
    assert.equal(
      localEnvLines[1],
      `TAURI_SIGNING_PRIVATE_KEY_PATH='${privateKey}'`
    )
    assert.match(localEnvLines[2], /^TAURI_SIGNING_PRIVATE_KEY_PASSWORD='.+$/)
    assert.ok(localEnvLines[2].endsWith("'"))
    const githubSecretsText = readFileSync(githubSecrets, "utf8")
    assert.match(githubSecretsText, /TAURI_SIGNING_PRIVATE_KEY/)
    assert.match(githubSecretsText, /TAURI_SIGNING_PRIVATE_KEY_PASSWORD/)
    assert.ok(githubSecretsText.includes(privateKey))
    assert.ok(githubSecretsText.includes(passwordFile))
    assert.ok(!githubSecretsText.includes(password))
    assert.ok(!githubSecretsText.includes("test-private-key"))

    const updatedConfig = JSON.parse(
      readFileSync(join(fixtureTauri, "tauri.conf.json"), "utf8")
    )
    assert.deepEqual(updatedConfig, {
      plugins: {
        updater: {
          pubkey: validPublicKey,
          endpoints: ["https://example.com/latest.json"],
        },
      },
    })

    const rerunResult = spawnSync(
      process.execPath,
      [realpathSync(fixtureScript)],
      {
        encoding: "utf8",
        env: baseEnv,
      }
    )
    assert.equal(rerunResult.status, 1)
    assert.equal(rerunResult.stdout, "")
    assert.equal(
      rerunResult.stderr,
      "updater signing files already exist; refusing to overwrite\n"
    )
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})
