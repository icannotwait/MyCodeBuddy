const FORK_VERSION_RE = /^\d+\.\d+\.\d+-mycodebuddy\.[1-9]\d*$/
const UPSTREAM_REPO_RE =
  /https:\/\/(?:github\.com|raw\.githubusercontent\.com)\/xintaofei\/codeg/gi
const ALLOWED_UPSTREAM_FILES = new Set(["NOTICE", "docs/UPSTREAM_SYNC.md"])
const WINDOWS_RELEASE_TARGETS = new Set(["x86_64-pc-windows-msvc"])
const UPDATER_SIGNING_SECRETS = [
  "TAURI_SIGNING_PRIVATE_KEY",
  "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
]
const FORK_REPOSITORY = "icannotwait/MyCodeBuddy"
const TAURI_RELEASE_ACTION_RE =
  /^\s*(?:-\s*)?uses\s*:\s*tauri-apps\/tauri-action(?:@|\s|$)/im
const TAURI_BUILD_COMMAND_RE = /\bpnpm\s+(?:exec\s+)?tauri\s+build\b/i

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

function workflowStepBlocks(workflowText) {
  const lines = workflowText.split(/\r?\n/)
  const starts = []
  for (let index = 0; index < lines.length; index += 1) {
    const match = lines[index].match(/^(\s*)-\s+/)
    if (match) starts.push({ index, indent: match[1].length })
  }

  return starts.map(({ index, indent }, startIndex) => {
    let end = lines.length
    for (
      let nextIndex = startIndex + 1;
      nextIndex < starts.length;
      nextIndex += 1
    ) {
      if (starts[nextIndex].indent <= indent) {
        end = starts[nextIndex].index
        break
      }
    }
    return lines.slice(index, end).join("\n")
  })
}

function hasExplicitWindowsMatrixTarget(stepText, targets) {
  const hasMatrixTarget = /\$\{\{\s*matrix\.target\s*\}\}/i.test(stepText)
  const hasTargetArgument =
    /--target(?:=|\s+)\s*["']?\$\{\{\s*matrix\.target\s*\}\}/i.test(stepText)
  const hasTargetProperty =
    /^\s*target\s*:\s*["']?\$\{\{\s*matrix\.target\s*\}\}/im.test(stepText)
  return (
    hasMatrixTarget &&
    (hasTargetArgument || hasTargetProperty) &&
    [...targets].some((target) => WINDOWS_RELEASE_TARGETS.has(target))
  )
}

function assertDirectUpdaterSigningEnvMappings(workflowText) {
  const mappingRe =
    /^[ \t]*(?:-[ \t]*)?([A-Za-z_][A-Za-z0-9_]*)[ \t]*:[ \t]*(.*?)[ \t]*(?:#.*)?$/gm
  const secretReferences = new Map(
    UPDATER_SIGNING_SECRETS.map((secret) => [
      secret,
      new RegExp(String.raw`\$\{\{\s*secrets\.${secret}\s*\}\}`),
    ])
  )

  for (const secret of UPDATER_SIGNING_SECRETS) {
    const directMappingRe = new RegExp(
      String.raw`^[ \t]*${secret}[ \t]*:[ \t]*\$\{\{\s*secrets\.${secret}\s*\}\}[ \t]*(?:#.*)?$`,
      "im"
    )
    if (!directMappingRe.test(workflowText)) {
      throw new Error(
        `release workflow requires direct env mapping for ${secret}`
      )
    }
  }

  for (const match of workflowText.matchAll(mappingRe)) {
    const [, name, value] = match
    for (const [secret, referenceRe] of secretReferences) {
      if (!referenceRe.test(value)) continue
      if (name !== secret) {
        throw new Error(
          `release workflow must not assign ${secret} to env alias ${name}`
        )
      }
    }
  }
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
  if (!/submodules\s*:\s*recursive/i.test(policyText)) {
    throw new Error("desktop release must checkout submodules recursively")
  }
  if (
    !/oven-sh\/setup-bun@v2/i.test(policyText) ||
    !/bun-version\s*:\s*1\.3\.14/i.test(policyText)
  ) {
    throw new Error("desktop release must pin Bun 1.3.14")
  }
  if (!policyText.includes("codex-acp-x86_64-pc-windows-msvc.exe")) {
    throw new Error("desktop release must verify the codex-acp x64 sidecar")
  }
  if (policyText.includes("CODEG_SKIP_CODEX_ACP_SIDECAR")) {
    throw new Error("desktop release must not skip the codex-acp sidecar")
  }
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

  if (
    /^\s*(?:runs-on|runner|os)\s*:[^\n]*\bmacos-[^\s"'#]+/im.test(policyText)
  ) {
    throw new Error("release workflow contains a macOS runner")
  }

  assertDirectUpdaterSigningEnvMappings(policyText)

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

  for (const stepText of workflowStepBlocks(policyText)) {
    const isTauriReleaseInvocation =
      TAURI_RELEASE_ACTION_RE.test(stepText) ||
      TAURI_BUILD_COMMAND_RE.test(stepText)
    if (
      isTauriReleaseInvocation &&
      !hasExplicitWindowsMatrixTarget(stepText, targets)
    ) {
      throw new Error(
        "release Tauri build must use an allowed Windows matrix target"
      )
    }
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

export function assertUpdaterArtifactPolicy({
  defaultConfig,
  releaseConfig,
  workflowText,
}) {
  if (defaultConfig.bundle?.createUpdaterArtifacts !== false) {
    throw new Error(
      "default Tauri config must set bundle.createUpdaterArtifacts to false"
    )
  }
  if (releaseConfig.bundle?.createUpdaterArtifacts !== true) {
    throw new Error(
      "release Tauri config must set bundle.createUpdaterArtifacts to true"
    )
  }

  const releaseConfigArgument =
    /--config\s+["']?src-tauri\/tauri\.release\.conf\.json["']?/i
  const updaterJsonEnabled = /^\s*includeUpdaterJson\s*:\s*true\s*$/im
  const releaseSteps = workflowStepBlocks(
    uncommentedWorkflowText(workflowText)
  ).filter((stepText) => TAURI_RELEASE_ACTION_RE.test(stepText))

  if (releaseSteps.length === 0) {
    throw new Error("release workflow has no Tauri desktop release step")
  }
  for (const stepText of releaseSteps) {
    if (!releaseConfigArgument.test(stepText)) {
      throw new Error(
        "release Tauri build must use src-tauri/tauri.release.conf.json"
      )
    }
    if (!updaterJsonEnabled.test(stepText)) {
      throw new Error("release Tauri build must set includeUpdaterJson: true")
    }
  }
}

export function assertServerInstallerCompliance(installScriptText) {
  if (
    !/\$RequiredInstalledFiles\s*=\s*@\("codeg-server\.exe",\s*"codeg-mcp\.exe",\s*"LICENSE",\s*"NOTICE",\s*"THIRD_PARTY_LICENSES\.txt"\)/.test(
      installScriptText
    )
  ) {
    throw new Error(
      "server installer required installed files must include both executables and all compliance files"
    )
  }
  if (!installScriptText.includes('$RequiredWebFiles = @("web\\index.html")')) {
    throw new Error(
      "server installer required web entry must be web/index.html"
    )
  }
  if (
    !/function Test-NonEmptyRegularFile\(\[string\]\$Path\)\s*\{[\s\S]*?\$item\s*=\s*Get-Item -LiteralPath \$Path -Force -ErrorAction SilentlyContinue[\s\S]*?\$item -is \[System\.IO\.FileInfo\][\s\S]*?\$item\.Length -gt 0[\s\S]*?\}/.test(
      installScriptText
    )
  ) {
    throw new Error("server installer must validate a nonempty regular file")
  }
  const installedCompleteness = installScriptText.match(
    /function Test-InstalledFilesComplete\(\[string\]\$Directory\)\s*\{[\s\S]*?# ── Resolve version ──/
  )?.[0]
  if (
    !installedCompleteness ||
    !/foreach \(\$filename in \$RequiredInstalledFiles\)[\s\S]*?Test-NonEmptyRegularFile -Path \$path/.test(
      installedCompleteness
    ) ||
    !/foreach \(\$relativePath in \$RequiredWebFiles\)[\s\S]*?Test-NonEmptyRegularFile -Path \$path/.test(
      installedCompleteness
    )
  ) {
    throw new Error(
      "server installer must check every required installed file and web entry as a nonempty regular file"
    )
  }

  const versionShortcut = installScriptText.match(
    /if \(\$CurrentVersion[\s\S]*?Write-Host "codeg-server is already at version \$TargetVer, nothing to do\."[\s\S]*?exit 0\s*\}/
  )?.[0]
  if (
    !versionShortcut ||
    !/-and \(Test-InstalledFilesComplete -Directory \$InstallDir\)/.test(
      versionShortcut
    )
  ) {
    throw new Error(
      "server installer version shortcut must require complete installed files"
    )
  }

  const installMarker = installScriptText.indexOf("# ── Install ──")
  if (installMarker < 0) {
    throw new Error("server installer is missing the install section")
  }
  const installSection = installScriptText.slice(installMarker)
  const firstInstallWrite = installSection.indexOf(
    "New-Item -ItemType Directory -Force -Path $InstallDir"
  )
  if (firstInstallWrite < 0) {
    throw new Error("server installer does not create InstallDir")
  }

  const validationBlocks = [
    "foreach ($name in $ManagedBins)",
    "foreach ($filename in $ComplianceFiles)",
    "foreach ($relativePath in $RequiredWebFiles)",
  ]
  for (const block of validationBlocks) {
    const blockIndex = installSection.indexOf(block)
    if (blockIndex < 0 || blockIndex >= firstInstallWrite) {
      throw new Error(
        "server installer must validate binaries, compliance files, and web/index.html before writing InstallDir"
      )
    }
  }
  if (
    !/\$ManagedBins\s*=\s*@\("codeg-server",\s*"codeg-mcp"\)/.test(
      installScriptText
    )
  ) {
    throw new Error(
      "server installer must manage codeg-server.exe and codeg-mcp.exe"
    )
  }
  if (
    !/\$ComplianceFiles\s*=\s*@\("LICENSE",\s*"NOTICE",\s*"THIRD_PARTY_LICENSES\.txt"\)/.test(
      installScriptText
    )
  ) {
    throw new Error(
      "server installer must manage LICENSE, NOTICE, and THIRD_PARTY_LICENSES.txt"
    )
  }

  const validationPrefix = installSection.slice(0, firstInstallWrite)
  if (
    !/foreach \(\$name in \$ManagedBins\)[\s\S]*?Test-NonEmptyRegularFile -Path \$src/.test(
      validationPrefix
    ) ||
    !/foreach \(\$filename in \$ComplianceFiles\)[\s\S]*?Test-NonEmptyRegularFile -Path \$src/.test(
      validationPrefix
    ) ||
    !/foreach \(\$relativePath in \$RequiredWebFiles\)[\s\S]*?Join-Path \$TmpDir \$Artifact \$relativePath[\s\S]*?Test-NonEmptyRegularFile -Path \$src/.test(
      validationPrefix
    )
  ) {
    throw new Error(
      "server installer must validate nonempty binaries, compliance files, and web/index.html before writing InstallDir"
    )
  }
  const writeSection = installSection.slice(firstInstallWrite)
  if (
    !/foreach \(\$filename in \$ComplianceFiles\)[\s\S]*?Copy-Item -LiteralPath \$src -Destination \$dst -Force/.test(
      writeSection
    )
  ) {
    throw new Error(
      "server installer must copy all compliance files to InstallDir"
    )
  }
}

function isAuthenticodeConfigKey(key) {
  const normalized = key.replace(/[-_.]/g, "").toLowerCase()
  return (
    normalized.includes("authenticode") ||
    normalized.startsWith("certificate") ||
    normalized === "digestalgorithm" ||
    normalized.startsWith("timestamp") ||
    normalized === "signcommand" ||
    normalized === "signingcommand" ||
    normalized.startsWith("signtool") ||
    normalized.startsWith("signingcertificate") ||
    normalized.startsWith("signingtool") ||
    normalized.startsWith("windowscertificate") ||
    normalized.startsWith("windowssign") ||
    normalized.startsWith("windowsdigest") ||
    normalized.startsWith("windowstimestamp") ||
    normalized.startsWith("trustedsigning") ||
    normalized.startsWith("azuretrusted")
  )
}

export function assertNoAuthenticodeConfig(tauriConfig) {
  const visit = (value, path) => {
    if (!value || typeof value !== "object") return
    if (Array.isArray(value)) {
      value.forEach((item, index) => visit(item, `${path}[${index}]`))
      return
    }

    for (const [key, child] of Object.entries(value)) {
      if (isAuthenticodeConfigKey(key)) {
        throw new Error(
          `Tauri configuration must not contain Authenticode key ${path}.${key}`
        )
      }
      visit(child, `${path}.${key}`)
    }
  }

  visit(tauriConfig, "tauriConfig")
}
