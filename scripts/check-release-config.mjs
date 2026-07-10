import { readFile } from "node:fs/promises"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import {
  assertComplianceResources,
  assertMatchingVersions,
  assertWindowsReleaseWorkflow,
  findForbiddenRuntimeUrls,
  readCargoVersion,
} from "./release-policy.mjs"

const root = join(dirname(fileURLToPath(import.meta.url)), "..")
const args = process.argv.slice(2)

if (
  args.length > 2 ||
  (args.length > 0 && (args[0] !== "--tag" || !args[1]))
) {
  throw new Error("usage: node scripts/check-release-config.mjs [--tag vVERSION]")
}

const tag = args[1]
const readRootFile = (path) => readFile(join(root, path), "utf8")
const [
  packageText,
  cargoText,
  tauriConfigText,
  workflowText,
  versionText,
  settingsText,
  installScriptText,
] = await Promise.all([
  readRootFile("package.json"),
  readRootFile("src-tauri/Cargo.toml"),
  readRootFile("src-tauri/tauri.conf.json"),
  readRootFile(".github/workflows/release.yml"),
  readRootFile("src-tauri/src/update/version.rs"),
  readRootFile("src/components/settings/system-network-settings.tsx"),
  readRootFile("install.ps1"),
])

const packageConfig = JSON.parse(packageText)
const tauriConfig = JSON.parse(tauriConfigText)

assertMatchingVersions({
  packageVersion: packageConfig.version,
  cargoVersion: readCargoVersion(cargoText),
  tauriVersion: tauriConfig.version,
  tag,
})
assertWindowsReleaseWorkflow(workflowText)
assertComplianceResources(tauriConfig)

const forbiddenFiles = findForbiddenRuntimeUrls({
  "src-tauri/tauri.conf.json": tauriConfigText,
  "src-tauri/src/update/version.rs": versionText,
  "src/components/settings/system-network-settings.tsx": settingsText,
  "install.ps1": installScriptText,
})

if (forbiddenFiles.length) {
  throw new Error(
    `forbidden upstream runtime URLs found in: ${forbiddenFiles.join(", ")}`
  )
}

console.log("Release configuration is valid.")
