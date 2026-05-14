#
# Codeg Server installer for Windows
# Usage:
#   irm https://raw.githubusercontent.com/xintaofei/codeg/main/install.ps1 | iex
#   .\install.ps1 -Version v0.5.0
#

param(
    [string]$Version = "",
    [string]$InstallDir = "$env:LOCALAPPDATA\codeg",
    [switch]$NoCleanup
)

$ErrorActionPreference = "Stop"
$Repo = "xintaofei/codeg"
$Artifact = "codeg-server-windows-x64"

# Stale codeg-server binaries elsewhere in PATH are removed by default so the
# user's `codeg-server` command always runs the freshly installed binary. Pass
# -NoCleanup (or set CODEG_NO_CLEANUP=1) to disable.
$Cleanup = -not $NoCleanup
if ($env:CODEG_NO_CLEANUP -eq "1") {
    $Cleanup = $false
}

function Get-CanonicalPath([string]$Path) {
    if (-not $Path) { return "" }
    try {
        return (Resolve-Path -LiteralPath $Path -ErrorAction Stop).Path
    } catch {
        return $Path
    }
}

function Read-BinVersion([string]$BinPath) {
    if (-not (Test-Path -LiteralPath $BinPath)) { return "" }
    $stdout = Join-Path $env:TEMP ("codeg-ver-" + [Guid]::NewGuid().ToString() + ".txt")
    $stderr = Join-Path $env:TEMP ("codeg-vererr-" + [Guid]::NewGuid().ToString() + ".txt")
    try {
        $proc = Start-Process -FilePath $BinPath -ArgumentList "--version" `
            -NoNewWindow -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        $exited = $proc.WaitForExit(3000)
        if (-not $exited) { try { $proc.Kill() } catch {} }
        if (Test-Path $stdout) {
            $line = (Get-Content $stdout -ErrorAction SilentlyContinue | Select-Object -First 1)
            if ($line) { return $line.Trim() }
        }
        return ""
    } catch {
        return ""
    } finally {
        Remove-Item $stdout -Force -ErrorAction SilentlyContinue
        Remove-Item $stderr -Force -ErrorAction SilentlyContinue
    }
}

# ── Resolve version ──

if (-not $Version) {
    Write-Host "Fetching latest release..."
    $release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
    $Version = $release.tag_name
    if (-not $Version) {
        Write-Error "Could not determine latest version"
        exit 1
    }
}

$TargetVer = $Version -replace '^v', ''

# ── Scan PATH for every codeg-server[.exe] binary ──

$DestBin = Join-Path $InstallDir "codeg-server.exe"
$DestBinReal = Get-CanonicalPath $DestBin

$PathConflicts = @()
$seenReal = @{}
$pathDirs = @()
if ($env:Path) { $pathDirs = $env:Path.Split(';') }
foreach ($dir in $pathDirs) {
    if (-not $dir) { continue }
    foreach ($leaf in @("codeg-server.exe", "codeg-server")) {
        $bin = Join-Path $dir $leaf
        if (Test-Path -LiteralPath $bin -PathType Leaf) {
            $real = Get-CanonicalPath $bin
            if ($seenReal.ContainsKey($real)) { continue }
            $seenReal[$real] = $true
            if ($real -ne $DestBinReal) {
                $PathConflicts += $bin
            }
        }
    }
}

# What does `codeg-server` actually resolve to in the current PATH?
$ActiveBin = ""
$resolved = Get-Command codeg-server -ErrorAction SilentlyContinue
if ($resolved) { $ActiveBin = $resolved.Source }

# ── Version detection — prefer the binary the user actually invokes ──

$VersionCheckBin = ""
if ($ActiveBin -and (Test-Path -LiteralPath $ActiveBin)) {
    $VersionCheckBin = $ActiveBin
} elseif (Test-Path -LiteralPath $DestBin) {
    $VersionCheckBin = $DestBin
}

$CurrentVersion = ""
$WasRunning = $false
if ($VersionCheckBin) {
    $CurrentVersion = Read-BinVersion $VersionCheckBin
}

# Only short-circuit when the active binary is up to date AND the destination
# itself has it AND no other PATH entries shadow it.
if ($CurrentVersion -and ($CurrentVersion -eq $TargetVer) `
        -and ($PathConflicts.Count -eq 0) `
        -and (Test-Path -LiteralPath $DestBin)) {
    Write-Host "codeg-server is already at version $TargetVer, nothing to do."
    exit 0
}

if ($CurrentVersion) {
    Write-Host "Upgrading codeg-server: $CurrentVersion -> $TargetVer..."
} else {
    Write-Host "Installing codeg-server $Version (windows/x64)..."
}

# ── Warn about codeg-server binaries shadowing the target install ──

if ($PathConflicts.Count -gt 0) {
    Write-Host ""
    Write-Host "Found other codeg-server binaries in PATH that may shadow ${DestBin}:"
    foreach ($c in $PathConflicts) {
        $cv = Read-BinVersion $c
        if ($cv) {
            Write-Host "  - $c  (version $cv)"
        } else {
            Write-Host "  - $c"
        }
    }
    if ($Cleanup) {
        Write-Host "These will be removed after installation. Pass -NoCleanup to keep them."
    } else {
        Write-Host "Keeping them (-NoCleanup). You may need to remove them manually so that"
        Write-Host "typing 'codeg-server' runs the new install at $DestBin."
    }
    Write-Host ""
}

# ── Stop running service before upgrade ──

$ServerProcesses = Get-Process -Name "codeg-server" -ErrorAction SilentlyContinue
if ($ServerProcesses) {
    Write-Host "Stopping running codeg-server process(es)..."
    $WasRunning = $true
    $ServerProcesses | Stop-Process -Force
    Start-Sleep -Seconds 2
    # Verify stopped
    $StillRunning = Get-Process -Name "codeg-server" -ErrorAction SilentlyContinue
    if ($StillRunning) {
        $StillRunning | Stop-Process -Force
        Start-Sleep -Seconds 1
    }
    Write-Host "codeg-server stopped."
}

# ── Download and extract ──

$Url = "https://github.com/$Repo/releases/download/$Version/$Artifact.zip"
$TmpDir = Join-Path $env:TEMP "codeg-install-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null
$ZipPath = Join-Path $TmpDir "$Artifact.zip"

Write-Host "Downloading $Url..."
try {
    Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing
} catch {
    Write-Error "Download failed. Check that version $Version exists and has a $Artifact asset."
    exit 1
}

Write-Host "Extracting..."
Expand-Archive -Path $ZipPath -DestinationPath $TmpDir -Force

# ── Install ──

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$BinarySrc = Join-Path $TmpDir $Artifact "codeg-server.exe"
if (-not (Test-Path $BinarySrc)) {
    Write-Error "Binary not found in archive"
    exit 1
}
Copy-Item $BinarySrc -Destination (Join-Path $InstallDir "codeg-server.exe") -Force

# Install web assets
$WebSrc = Join-Path $TmpDir $Artifact "web"
$WebDir = Join-Path $InstallDir "web"
if (Test-Path $WebSrc) {
    Write-Host "Installing web assets to $WebDir..."
    if (Test-Path $WebDir) { Remove-Item $WebDir -Recurse -Force }
    Copy-Item $WebSrc -Destination $WebDir -Recurse
}

# ── Add to PATH ──

$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
    Write-Host "Added $InstallDir to user PATH (restart terminal to take effect)"
}

# ── Cleanup ──

Remove-Item $TmpDir -Recurse -Force -ErrorAction SilentlyContinue

# ── Remove conflicting binaries from other PATH locations ──

if ($Cleanup -and $PathConflicts.Count -gt 0) {
    Write-Host ""
    Write-Host "Removing stale codeg-server binaries..."
    foreach ($c in $PathConflicts) {
        try {
            Remove-Item -LiteralPath $c -Force -ErrorAction Stop
            Write-Host "  removed $c"
        } catch {
            Write-Host "  failed to remove $c — $($_.Exception.Message)"
        }
    }
}

# ── Restart service if it was running ──

if ($WasRunning) {
    Write-Host ""
    Write-Host "Note: codeg-server was stopped for the upgrade."
    Write-Host "Please restart it manually to ensure your environment variables (CODEG_PORT, CODEG_TOKEN, etc.) are preserved:"
    Write-Host "  `$env:CODEG_STATIC_DIR=`"$WebDir`"; codeg-server"
}

# ── Done ──

$InstalledVer = ""
try {
    $InstalledVer = (& (Join-Path $InstallDir "codeg-server.exe") --version 2>$null).Trim()
} catch {}
if (-not $InstalledVer) { $InstalledVer = $TargetVer }

Write-Host ""
Write-Host "codeg-server installed to $InstallDir\codeg-server.exe"
Write-Host "Version: $InstalledVer"

# Verify the user's `codeg-server` command actually resolves to the new binary.
$ActiveBinAfter = ""
$resolvedAfter = Get-Command codeg-server -ErrorAction SilentlyContinue
if ($resolvedAfter) { $ActiveBinAfter = $resolvedAfter.Source }
$ActiveBinAfterReal = Get-CanonicalPath $ActiveBinAfter

if (-not $ActiveBinAfter) {
    Write-Host ""
    Write-Host "Note: $InstallDir is not on the current session's PATH."
    Write-Host "Open a new terminal (PATH was just updated) or run:"
    Write-Host "  `$env:Path = `"$InstallDir;`$env:Path`""
} elseif ($ActiveBinAfterReal -ne $DestBinReal) {
    Write-Host ""
    Write-Host "Warning: typing 'codeg-server' still runs $ActiveBinAfter, not $DestBin."
    Write-Host "Another binary earlier in PATH is shadowing the new install. To fix, either:"
    Write-Host "  - re-run without -NoCleanup (the default removes shadowing binaries), or"
    Write-Host "  - remove the stale binary manually: Remove-Item '$ActiveBinAfter', or"
    Write-Host "  - put $InstallDir before its directory in PATH."
}

Write-Host ""
Write-Host "Quick start:"
Write-Host "  `$env:CODEG_STATIC_DIR=`"$WebDir`"; codeg-server"
Write-Host ""
Write-Host "Or with custom settings:"
Write-Host "  `$env:CODEG_PORT=`"3080`"; `$env:CODEG_TOKEN=`"your-secret`"; `$env:CODEG_STATIC_DIR=`"$WebDir`"; codeg-server"
