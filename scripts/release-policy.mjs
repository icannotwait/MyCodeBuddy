const FORK_VERSION_RE = /^\d+\.\d+\.\d+-mycodebuddy\.\d+$/
const UPSTREAM_REPO_RE =
  /https:\/\/(?:github\.com|raw\.githubusercontent\.com)\/xintaofei\/codeg/gi
const ALLOWED_UPSTREAM_FILES = new Set(["NOTICE", "docs/UPSTREAM_SYNC.md"])

export function readCargoVersion(text) {
  const packageBlock = text.match(/\[package\]([\s\S]*?)(?=\n\[|$)/)?.[1]
  const version = packageBlock?.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1]
  if (!version) throw new Error("Cargo package version not found")
  return version
}

export function assertForkVersion(version) {
  if (!FORK_VERSION_RE.test(version)) {
    throw new Error(
      `version must match MAJOR.MINOR.PATCH-mycodebuddy.COUNTER: ${version}`
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
  for (const target of [
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
  ]) {
    if (!workflowText.includes(target)) {
      throw new Error(`missing Windows target ${target}`)
    }
  }
  for (const forbidden of [
    "apple-darwin",
    "unknown-linux",
    "APPLE_CERTIFICATE",
    "DOCKERHUB_",
    "build-docker",
  ]) {
    if (workflowText.includes(forbidden)) {
      throw new Error(`release workflow contains non-Windows entry ${forbidden}`)
    }
  }
}

export function assertComplianceResources(tauriConfig) {
  const bundle = tauriConfig.bundle ?? {}
  const resources = bundle.resources ?? {}
  const expected = {
    "../LICENSE": "licenses/LICENSE",
    "../NOTICE": "licenses/NOTICE",
    "resources/THIRD_PARTY_LICENSES.txt":
      "licenses/THIRD_PARTY_LICENSES.txt",
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
