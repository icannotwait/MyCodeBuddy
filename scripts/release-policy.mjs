const FORK_VERSION_RE = /^\d+\.\d+\.\d+-mycodebuddy\.[1-9]\d*$/
const UPSTREAM_REPO_RE =
  /https:\/\/(?:github\.com|raw\.githubusercontent\.com)\/xintaofei\/codeg/gi
const ALLOWED_UPSTREAM_FILES = new Set(["NOTICE", "docs/UPSTREAM_SYNC.md"])
const WINDOWS_RELEASE_TARGETS = new Set([
  "x86_64-pc-windows-msvc",
  "aarch64-pc-windows-msvc",
])
const UPDATER_SIGNING_SECRETS = [
  "TAURI_SIGNING_PRIVATE_KEY",
  "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
]
const FORK_REPOSITORY = "icannotwait/MyCodeBuddy"

function uncommentedWorkflowText(workflowText) {
  return workflowText
    .split(/\r?\n/)
    .filter((line) => !/^\s*#/.test(line))
    .join("\n")
}

function releaseTargets(workflowText) {
  const targets = new Set()
  const targetPropertyRe =
    /^\s*(?:-\s*)?target\s*:\s*(?:"([^"]+)"|'([^']+)'|([^\s#]+))/gm
  const targetArgumentRe =
    /--target(?:=|\s+)(?:"([a-z0-9_.-]+)"|'([a-z0-9_.-]+)'|([a-z0-9_.-]+))/gi
  const targetTripleRe =
    /\b(?:aarch64|arm(?:64)?|armv[4-9][a-z0-9_]*|i[3-6]86|loongarch64|mips(?:64)?(?:el)?|powerpc(?:64(?:le)?)?|riscv(?:32|64)[a-z0-9_]*|s390x|sparc64|thumbv[0-9a-z_]+|wasm(?:32|64)|x86_64)-[a-z0-9_]+-[a-z0-9_]+(?:-[a-z0-9_]+)?\b/gi
  const addTarget = (target) => {
    if (/^[a-z0-9_]+(?:-[a-z0-9_]+){2,}$/i.test(target)) {
      targets.add(target)
    }
  }

  for (const match of workflowText.matchAll(targetPropertyRe)) {
    addTarget(match[1] ?? match[2] ?? match[3])
  }
  for (const match of workflowText.matchAll(targetArgumentRe)) {
    addTarget(match[1] ?? match[2] ?? match[3])
  }
  for (const match of workflowText.matchAll(targetTripleRe)) {
    targets.add(match[0])
  }
  return targets
}

function referencesGitHubSecret(workflowText, secret) {
  const referenceRe = new RegExp(String.raw`\$\{\{\s*secrets\.${secret}\s*\}\}`)
  return referenceRe.test(workflowText)
}

function printsUpdaterSigningValue(workflowText) {
  const outputCommandRe =
    /\b(?:echo|printf|printenv|Write-Host|Write-Output|console\.log|core\.(?:debug|error|info|notice|warning)|process\.stdout\.write)\b/i
  const secretValueRe =
    /(?:\$\{\{\s*(?:env|secrets)\.TAURI_SIGNING_PRIVATE_KEY(?:_PASSWORD)?\s*\}\}|\$(?:env:)?TAURI_SIGNING_PRIVATE_KEY(?:_PASSWORD)?\b|\$\{TAURI_SIGNING_PRIVATE_KEY(?:_PASSWORD)?[^}]*\}|process\.env(?:\.|\[["'])TAURI_SIGNING_PRIVATE_KEY(?:_PASSWORD)?|toJSON\(\s*secrets\s*\))/i
  const printenvSecretRe =
    /\bprintenv\s+TAURI_SIGNING_PRIVATE_KEY(?:_PASSWORD)?\b/i

  return workflowText
    .split(/\r?\n/)
    .some(
      (line) =>
        printenvSecretRe.test(line) ||
        (outputCommandRe.test(line) && secretValueRe.test(line))
    )
}

export function readCargoVersion(text) {
  const packageBlock = text.match(/\[package\]([\s\S]*?)(?=\n\[|$)/)?.[1]
  const version = packageBlock?.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1]
  if (!version) throw new Error("Cargo package version not found")
  return version
}

export function assertForkVersion(version) {
  if (!FORK_VERSION_RE.test(version)) {
    throw new Error(
      `version must use a positive counter in MAJOR.MINOR.PATCH-mycodebuddy.COUNTER: ${version}`
    )
  }
}

export function assertMatchingVersions({
  packageVersion,
  cargoVersion,
  tauriVersion,
  tag,
}) {
  const versions = new Set([packageVersion, cargoVersion, tauriVersion])
  if (versions.size !== 1) {
    throw new Error(
      `version mismatch: package=${packageVersion}, cargo=${cargoVersion}, tauri=${tauriVersion}`
    )
  }
  assertForkVersion(packageVersion)
  if (tag && tag !== `v${packageVersion}`) {
    throw new Error(`tag ${tag} does not match v${packageVersion}`)
  }
}

export function findForbiddenRuntimeUrls(files) {
  return Object.entries(files)
    .filter(([name, text]) => {
      UPSTREAM_REPO_RE.lastIndex = 0
      return !ALLOWED_UPSTREAM_FILES.has(name) && UPSTREAM_REPO_RE.test(text)
    })
    .map(([name]) => name)
    .sort()
}

export function assertWindowsReleaseWorkflow(workflowText) {
  const policyText = uncommentedWorkflowText(workflowText)
  const targets = releaseTargets(policyText)
  for (const target of WINDOWS_RELEASE_TARGETS) {
    if (!targets.has(target)) {
      throw new Error(`missing Windows target ${target}`)
    }
  }
  for (const target of targets) {
    if (!WINDOWS_RELEASE_TARGETS.has(target)) {
      throw new Error(`unsupported release target ${target}`)
    }
  }

  for (const forbidden of [
    "apple-darwin",
    "unknown-linux",
    "APPLE_CERTIFICATE",
    "DOCKERHUB_",
    "build-docker",
  ]) {
    if (policyText.includes(forbidden)) {
      throw new Error(
        `release workflow contains non-Windows entry ${forbidden}`
      )
    }
  }

  for (const secret of UPDATER_SIGNING_SECRETS) {
    if (!referencesGitHubSecret(policyText, secret)) {
      throw new Error(`release workflow must reference secrets.${secret}`)
    }
  }

  const authenticodePatterns = [
    /^\s*(?:-\s*)?authenticode\s*:/im,
    /\b(?:signtool(?:\.exe)?|osslsigncode|trusted-signing-action)\b/i,
    /\b(?:certificate(?:Thumbprint|Sha1|Path|Password)|signCommand|timestampUrl)\b/i,
    /\b(?:WINDOWS_CERTIFICATE(?:_[A-Z0-9_]+)?|CSC_LINK|CSC_KEY_PASSWORD)\b/i,
    /\bTAURI_BUNDLER_WINDOWS_(?:CERTIFICATE|DIGEST|SIGN|TIMESTAMP|TSP)[A-Z0-9_]*\b/i,
    /\b(?:AZURE_TRUSTED_SIGNING|WINDOWS_SIGNING)[A-Z0-9_]*\b/i,
  ]
  if (authenticodePatterns.some((pattern) => pattern.test(policyText))) {
    throw new Error("release workflow must not configure Authenticode signing")
  }

  if (printsUpdaterSigningValue(policyText)) {
    throw new Error(
      "release workflow must not print updater private-key or password values"
    )
  }

  const prereleaseValues = [
    ...policyText.matchAll(/^\s*prerelease\s*:\s*([^,\s#}]+)/gim),
  ].map((match) => match[1].toLowerCase())
  if (
    prereleaseValues.length === 0 ||
    prereleaseValues.some((value) => value !== "false")
  ) {
    throw new Error("GitHub release must set prerelease: false")
  }

  const escapedRepository = FORK_REPOSITORY.replace("/", String.raw`\/`)
  const repositoryMetadataRe = new RegExp(
    String.raw`^\s*(?:[A-Z0-9_-]*REPOSITORY|repo)\s*:\s*["']?${escapedRepository}["']?\s*(?:#.*)?$`,
    "im"
  )
  const repositoryCheckRe = new RegExp(
    String.raw`(?:github\.repository|GITHUB_REPOSITORY)[^\n]*${escapedRepository}|${escapedRepository}[^\n]*(?:github\.repository|GITHUB_REPOSITORY)`,
    "i"
  )
  if (
    !repositoryMetadataRe.test(policyText) &&
    !repositoryCheckRe.test(policyText)
  ) {
    throw new Error(
      `release workflow must identify fork repository ${FORK_REPOSITORY}`
    )
  }
}

export function assertComplianceResources(tauriConfig) {
  const bundle = tauriConfig.bundle ?? {}
  const resources = bundle.resources ?? {}
  const expected = {
    "../LICENSE": "licenses/LICENSE",
    "../NOTICE": "licenses/NOTICE",
    "resources/THIRD_PARTY_LICENSES.txt": "licenses/THIRD_PARTY_LICENSES.txt",
  }
  if (bundle.license !== "Apache-2.0") {
    throw new Error("bundle.license must be Apache-2.0")
  }
  if (bundle.licenseFile !== "../LICENSE") {
    throw new Error("bundle.licenseFile must be ../LICENSE")
  }
  for (const [source, target] of Object.entries(expected)) {
    if (resources[source] !== target) {
      throw new Error(`missing compliance resource ${source} -> ${target}`)
    }
  }
}
