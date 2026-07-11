import { execFileSync } from "node:child_process"
import { randomBytes } from "node:crypto"
import {
  chmodSync,
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs"
import { homedir } from "node:os"
import { basename, dirname, join, resolve } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

const scriptPath = fileURLToPath(import.meta.url)
const repoRoot = resolve(dirname(scriptPath), "..")
const tauriConfigPath = join(repoRoot, "src-tauri", "tauri.conf.json")
const GENERATION_FAILURE_MESSAGE = "updater signing key generation failed"
const LOCK_MESSAGE = "updater signing generation is already in progress"
const OVERWRITE_MESSAGE =
  "updater signing files already exist; refusing to overwrite"

class SafeGenerationError extends Error {}

export function buildSigningPaths(homeDirectory) {
  const signingDir = join(homeDirectory, ".config", "mycodebuddy", "signing")

  return {
    signingDir,
    privateKey: join(signingDir, "updater-signing.key"),
    publicKey: join(signingDir, "updater-signing.key.pub"),
    passwordFile: join(signingDir, "updater-signing.password"),
    localEnv: join(signingDir, "local-build.env"),
    githubSecrets: join(signingDir, "GITHUB_SECRETS.md"),
  }
}

export function updatePublicKey(config, publicKey) {
  if (
    !config ||
    typeof config !== "object" ||
    !config.plugins ||
    typeof config.plugins !== "object" ||
    !config.plugins.updater ||
    typeof config.plugins.updater !== "object"
  ) {
    throw new Error("tauri config is missing plugins.updater")
  }

  return {
    ...config,
    plugins: {
      ...config.plugins,
      updater: {
        ...config.plugins.updater,
        pubkey: publicKey,
      },
    },
  }
}

export function replacePublicKeyInConfigText(configText, publicKey) {
  const config = JSON.parse(configText)
  const currentPublicKey = config?.plugins?.updater?.pubkey
  if (typeof currentPublicKey !== "string") {
    throw new Error("tauri config is missing plugins.updater.pubkey")
  }

  const escapedCurrentKey = JSON.stringify(currentPublicKey).replace(
    /[.*+?^${}()|[\]\\]/g,
    "\\$&"
  )
  const pubkeyPattern = new RegExp(
    `("pubkey"\\s*:\\s*)${escapedCurrentKey}`,
    "g"
  )
  const matches = [...configText.matchAll(pubkeyPattern)]
  if (matches.length !== 1) {
    throw new Error("tauri config updater pubkey could not be located uniquely")
  }

  const updatedText = configText.replace(
    pubkeyPattern,
    (_, prefix) => `${prefix}${JSON.stringify(publicKey)}`
  )
  const expectedConfig = updatePublicKey(config, publicKey)
  if (
    JSON.stringify(JSON.parse(updatedText)) !== JSON.stringify(expectedConfig)
  ) {
    throw new Error("tauri config update changed fields outside updater pubkey")
  }

  return updatedText
}

function isCanonicalBase64(value) {
  return (
    typeof value === "string" &&
    /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(
      value
    ) &&
    Buffer.from(value, "base64").toString("base64") === value
  )
}

export function validateGeneratedPublicKey(publicKey) {
  if (!isCanonicalBase64(publicKey)) {
    throw new Error("generated updater public key is invalid")
  }

  const document = Buffer.from(publicKey, "base64").toString("utf8")
  const lines = document.split("\n")
  if (
    lines.length !== 3 ||
    !/^untrusted comment: minisign public key: [0-9A-F]{16}$/.test(lines[0]) ||
    lines[2] !== "" ||
    !lines[1].startsWith("RW")
  ) {
    throw new Error("generated updater public key is invalid")
  }

  const keyPayload = lines[1]
  if (
    !isCanonicalBase64(keyPayload) ||
    Buffer.from(keyPayload, "base64").length !== 42
  ) {
    throw new Error("generated updater public key is invalid")
  }

  return publicKey
}

function shellQuote(value) {
  return `'${value.replaceAll("'", "'\"'\"'")}'`
}

function writeSupportFile(filePath, contents) {
  writeFileSync(filePath, contents, {
    encoding: "utf8",
    flag: "wx",
    mode: 0o600,
  })
  chmodSync(filePath, 0o600)
}

function writeConfigAtomically(configPath, contents) {
  const configDirectory = dirname(configPath)
  const configMode = statSync(configPath).mode & 0o777
  const temporaryPath = join(
    configDirectory,
    `.${basename(configPath)}.${process.pid}.${randomBytes(8).toString("hex")}.tmp`
  )
  let temporaryFileExists = false

  try {
    writeFileSync(temporaryPath, contents, {
      encoding: "utf8",
      flag: "wx",
      mode: configMode,
    })
    temporaryFileExists = true
    renameSync(temporaryPath, configPath)
    temporaryFileExists = false
  } finally {
    if (temporaryFileExists) {
      rmSync(temporaryPath, { force: true })
    }
  }
}

function prepareUpdaterSigningKey() {
  const paths = buildSigningPaths(homedir())
  const lockPath = join(paths.signingDir, ".updater-signing-generation.lock")
  let lockAcquired = false
  let failure

  try {
    mkdirSync(paths.signingDir, { recursive: true, mode: 0o700 })
    chmodSync(paths.signingDir, 0o700)
    try {
      writeFileSync(lockPath, "", { flag: "wx", mode: 0o600 })
      lockAcquired = true
    } catch (error) {
      if (error && typeof error === "object" && error.code === "EEXIST") {
        throw new SafeGenerationError(LOCK_MESSAGE)
      }
      throw error
    }

    const generatedPaths = [
      paths.privateKey,
      paths.publicKey,
      paths.passwordFile,
      paths.localEnv,
      paths.githubSecrets,
    ]
    if (generatedPaths.some((generatedPath) => existsSync(generatedPath))) {
      throw new SafeGenerationError(OVERWRITE_MESSAGE)
    }

    const password = randomBytes(32).toString("base64url")
    process.env.PNPM_CONFIG_REPORTER = "silent"
    execFileSync(
      process.platform === "win32" ? "pnpm.cmd" : "pnpm",
      [
        "tauri",
        "signer",
        "generate",
        "--ci",
        "-p",
        password,
        "-w",
        paths.privateKey,
      ],
      { cwd: repoRoot, stdio: ["ignore", "ignore", "inherit"] }
    )

    const publicKey = validateGeneratedPublicKey(
      readFileSync(paths.publicKey, "utf8").trim()
    )
    const resolvedPrivateKeyPath = resolve(paths.privateKey)

    writeSupportFile(paths.passwordFile, password)
    writeSupportFile(
      paths.localEnv,
      [
        `TAURI_SIGNING_PRIVATE_KEY=${shellQuote(resolvedPrivateKeyPath)}`,
        `TAURI_SIGNING_PRIVATE_KEY_PATH=${shellQuote(resolvedPrivateKeyPath)}`,
        `TAURI_SIGNING_PRIVATE_KEY_PASSWORD=${shellQuote(password)}`,
      ].join("\n")
    )
    writeSupportFile(
      paths.githubSecrets,
      [
        "# GitHub updater signing secrets",
        "",
        `- TAURI_SIGNING_PRIVATE_KEY: use the contents of ${paths.privateKey}`,
        `- TAURI_SIGNING_PRIVATE_KEY_PASSWORD: use the contents of ${paths.passwordFile}`,
        "",
      ].join("\n")
    )

    const configText = readFileSync(tauriConfigPath, "utf8")
    writeConfigAtomically(
      tauriConfigPath,
      replacePublicKeyInConfigText(configText, publicKey)
    )
  } catch (error) {
    failure = error
  } finally {
    if (existsSync(paths.privateKey)) {
      try {
        chmodSync(paths.privateKey, 0o600)
      } catch (error) {
        failure ??= error
      }
    }

    if (lockAcquired) {
      try {
        rmSync(lockPath, { force: true })
      } catch (error) {
        failure ??= error
      }
    }
  }

  if (failure) {
    if (failure instanceof SafeGenerationError) {
      throw failure
    }
    throw new Error(GENERATION_FAILURE_MESSAGE)
  }

  for (const generatedPath of [
    paths.privateKey,
    paths.publicKey,
    paths.passwordFile,
    paths.localEnv,
    paths.githubSecrets,
    tauriConfigPath,
  ]) {
    console.log(generatedPath)
  }
  console.log("TAURI_SIGNING_PRIVATE_KEY")
  console.log("TAURI_SIGNING_PRIVATE_KEY_PASSWORD")
}

const isMain =
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href

if (isMain) {
  try {
    prepareUpdaterSigningKey()
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error))
    process.exitCode = 1
  }
}
