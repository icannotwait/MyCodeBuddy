import { execFileSync } from "node:child_process"
import { randomBytes } from "node:crypto"
import {
  chmodSync,
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs"
import { homedir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

const scriptPath = fileURLToPath(import.meta.url)
const repoRoot = resolve(dirname(scriptPath), "..")
const tauriConfigPath = join(repoRoot, "src-tauri", "tauri.conf.json")

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

function shellQuote(value) {
  return `'${value.replaceAll("'", "'\"'\"'")}'`
}

function prepareUpdaterSigningKey() {
  const paths = buildSigningPaths(homedir())
  mkdirSync(paths.signingDir, { recursive: true, mode: 0o700 })
  chmodSync(paths.signingDir, 0o700)

  const generatedPaths = [
    paths.privateKey,
    paths.publicKey,
    paths.passwordFile,
    paths.localEnv,
    paths.githubSecrets,
  ]
  if (generatedPaths.some((generatedPath) => existsSync(generatedPath))) {
    throw new Error(
      `refusing to overwrite existing updater signing files in ${paths.signingDir}`
    )
  }

  const password = randomBytes(32).toString("base64url")

  process.env.PNPM_CONFIG_REPORTER = "silent"
  try {
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
  } catch {
    throw new Error("updater signer generation failed")
  }

  chmodSync(paths.privateKey, 0o600)
  writeFileSync(paths.passwordFile, `${password}\n`, { mode: 0o600 })
  chmodSync(paths.passwordFile, 0o600)

  const publicKey = readFileSync(paths.publicKey, "utf8").trim()
  if (!publicKey) {
    throw new Error(`generated public key is empty: ${paths.publicKey}`)
  }

  const configText = readFileSync(tauriConfigPath, "utf8")
  writeFileSync(
    tauriConfigPath,
    replacePublicKeyInConfigText(configText, publicKey)
  )

  writeFileSync(
    paths.localEnv,
    [
      `TAURI_SIGNING_PRIVATE_KEY_PATH=${shellQuote(resolve(paths.privateKey))}`,
      `TAURI_SIGNING_PRIVATE_KEY_PASSWORD=${shellQuote(password)}`,
      "",
    ].join("\n"),
    { mode: 0o600 }
  )
  chmodSync(paths.localEnv, 0o600)

  writeFileSync(
    paths.githubSecrets,
    [
      "# GitHub updater signing secrets",
      "",
      `- TAURI_SIGNING_PRIVATE_KEY: use the contents of ${paths.privateKey}`,
      `- TAURI_SIGNING_PRIVATE_KEY_PASSWORD: use the contents of ${paths.passwordFile}`,
      "",
    ].join("\n"),
    { mode: 0o600 }
  )
  chmodSync(paths.githubSecrets, 0o600)

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
