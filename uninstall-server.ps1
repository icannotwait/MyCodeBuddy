#
# Remove a local codeg-server install from Windows.
#
# Desktop DrawCode does NOT ship or start codeg-server.exe. Use this script
# when antivirus flagged a leftover standalone server install, or when you
# no longer want the optional remote HTTP/WebSocket process on this machine.
#
# Usage:
#   .\uninstall-server.ps1
#   .\uninstall-server.ps1 -InstallDir "$env:LOCALAPPDATA\codeg"
#   .\uninstall-server.ps1 -RemoveData   # also delete SQLite/uploads under InstallDir
#   irm https://raw.githubusercontent.com/icannotwait/MyCodeBuddy/main/uninstall-server.ps1 | iex
#

param(
    [string]$InstallDir = "$env:LOCALAPPDATA\codeg",
    [switch]$RemoveData,
    [switch]$NoCleanup
)

$ErrorActionPreference = "Stop"

# Binaries managed by install.ps1 / this uninstaller.
$ManagedBins = @("codeg-server", "codeg-mcp")
$ComplianceFiles = @("LICENSE", "NOTICE", "THIRD_PARTY_LICENSES.txt")

function Get-CanonicalPath([string]$Path) {
    if (-not $Path) { return "" }
    try {
        return (Resolve-Path -LiteralPath $Path -ErrorAction Stop).Path
    } catch {
        return $Path
    }
}

Write-Host "DrawCode / Codeg standalone server uninstaller"
Write-Host "InstallDir: $InstallDir"
Write-Host ""

# ── Stop running server process(es) ──
$ServerProcesses = Get-Process -Name "codeg-server" -ErrorAction SilentlyContinue
if ($ServerProcesses) {
    Write-Host "Stopping running codeg-server process(es)..."
    $ServerProcesses | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 1
    $StillRunning = Get-Process -Name "codeg-server" -ErrorAction SilentlyContinue
    if ($StillRunning) {
        throw "codeg-server is still running. Close it (or end the process) and rerun this script."
    }
    Write-Host "codeg-server stopped."
} else {
    Write-Host "No running codeg-server process found."
}

# ── Remove install directory contents ──
if (Test-Path -LiteralPath $InstallDir) {
    $DestCanonical = Get-CanonicalPath $InstallDir
    foreach ($name in $ManagedBins) {
        $binPath = Join-Path $InstallDir "$name.exe"
        if (Test-Path -LiteralPath $binPath) {
            Remove-Item -LiteralPath $binPath -Force
            Write-Host "Removed $binPath"
        }
    }
    foreach ($filename in $ComplianceFiles) {
        $path = Join-Path $InstallDir $filename
        if (Test-Path -LiteralPath $path) {
            Remove-Item -LiteralPath $path -Force
            Write-Host "Removed $path"
        }
    }
    $webDir = Join-Path $InstallDir "web"
    if (Test-Path -LiteralPath $webDir) {
        Remove-Item -LiteralPath $webDir -Recurse -Force
        Write-Host "Removed $webDir"
    }

    if ($RemoveData) {
        Write-Host "Removing remaining data under $InstallDir ..."
        Remove-Item -LiteralPath $InstallDir -Recurse -Force
        Write-Host "InstallDir deleted."
    } else {
        # If the directory is empty (or only leftover junk), remove it.
        $remaining = @(Get-ChildItem -LiteralPath $InstallDir -Force -ErrorAction SilentlyContinue)
        if ($remaining.Count -eq 0) {
            Remove-Item -LiteralPath $InstallDir -Force -ErrorAction SilentlyContinue
            Write-Host "Empty InstallDir removed."
        } else {
            Write-Host "Left non-binary data under $InstallDir (pass -RemoveData to delete it)."
        }
    }
} else {
    Write-Host "InstallDir does not exist: $InstallDir"
}

# ── PATH shadow cleanup (same idea as install.ps1) ──
if (-not $NoCleanup -and $env:CODEG_NO_CLEANUP -ne "1") {
    $pathEntries = ($env:PATH -split ';' | Where-Object { $_ -and $_.Trim() })
    $shadows = @()
    foreach ($entry in $pathEntries) {
        $candidate = Join-Path $entry "codeg-server.exe"
        if (Test-Path -LiteralPath $candidate) {
            $shadows += (Get-CanonicalPath $candidate)
        }
    }
    $shadows = $shadows | Select-Object -Unique
    if ($shadows.Count -gt 0) {
        Write-Host "Removing other codeg-server.exe copies found on PATH..."
        foreach ($c in $shadows) {
            try {
                # Skip if it was already under InstallDir and we deleted it.
                if (Test-Path -LiteralPath $c) {
                    Remove-Item -LiteralPath $c -Force
                    Write-Host "  removed $c"
                }
            } catch {
                Write-Host "  failed to remove $c — $($_.Exception.Message)"
            }
        }
    }
}

$resolved = Get-Command codeg-server -ErrorAction SilentlyContinue
if ($resolved) {
    Write-Host ""
    Write-Host "Warning: 'codeg-server' still resolves to $($resolved.Source)."
    Write-Host "Remove that file manually if antivirus still reports it."
} else {
    Write-Host ""
    Write-Host "Done. codeg-server is no longer on PATH."
}

Write-Host "Desktop users: install DrawCode from GitHub Releases (NSIS). That package does not include codeg-server."
Write-Host "Self-hosters who still need the server: Docker or source build with --features server (see README)."
