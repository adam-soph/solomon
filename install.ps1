<#
.SYNOPSIS
    Download and install the `hcc` HolyC compiler (solomon) on Windows.

.DESCRIPTION
    solomon ships a single, self-contained binary: `hcc.exe`. The standard library
    is embedded at build time, so there is nothing else to install. This script
    detects your architecture, downloads the matching prebuilt binary from the
    GitHub release, drops it into a per-user directory, and adds that directory to
    your user PATH.

    This is the native-Windows installer (PowerShell). On Linux, macOS, or a POSIX
    shell on Windows (Git Bash / MSYS2 / WSL) use install.sh instead.

.PARAMETER Version
    Release tag to install (e.g. v0.1.0). Defaults to the latest release, or the
    HCC_VERSION environment variable if set.

.PARAMETER Dir
    Directory to install into. Defaults to %LOCALAPPDATA%\hcc\bin, or the
    HCC_INSTALL_DIR environment variable if set.

.EXAMPLE
    irm https://raw.githubusercontent.com/adam-soph/solomon/main/install.ps1 | iex

.EXAMPLE
    .\install.ps1 -Version v0.1.0 -Dir C:\tools\bin
#>
[CmdletBinding()]
param(
    [string]$Version = $(if ($env:HCC_VERSION)     { $env:HCC_VERSION }     else { 'latest' }),
    [string]$Dir     = $(if ($env:HCC_INSTALL_DIR) { $env:HCC_INSTALL_DIR } else { '' })
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Repo = 'adam-soph/solomon'
$Bin  = 'hcc'

function Info { param([string]$Msg) Write-Host "==> $Msg" -ForegroundColor Cyan }
function Ok   { param([string]$Msg) Write-Host $Msg -ForegroundColor Green }

# --- architecture -> release asset ------------------------------------------------
# The release matrix builds x86_64 and i686 Windows binaries. Windows-on-ARM runs
# x64 binaries under emulation, so ARM64 falls back to the x86_64 build.
$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
switch ($arch) {
    'AMD64' { $asset = "$Bin-x86_64-pc-windows-msvc.exe" }
    'ARM64' {
        Write-Warning 'No native ARM64 build; installing the x86_64 binary (runs under Windows emulation).'
        $asset = "$Bin-x86_64-pc-windows-msvc.exe"
    }
    'x86'   { $asset = "$Bin-i686-pc-windows-msvc.exe" }
    default { throw "unsupported architecture: $arch" }
}

# --- where to install -------------------------------------------------------------
if ([string]::IsNullOrEmpty($Dir)) {
    $Dir = Join-Path $env:LOCALAPPDATA 'hcc\bin'
}
$dest = Join-Path $Dir "$Bin.exe"

# --- download URL -----------------------------------------------------------------
if ($Version -eq 'latest') {
    $url = "https://github.com/$Repo/releases/latest/download/$asset"
} else {
    $url = "https://github.com/$Repo/releases/download/$Version/$asset"
}

Info "installing hcc ($Version) for windows/$arch"
Info "asset:   $asset"
Info "from:    $url"
Info "into:    $dest"

# --- download + install -----------------------------------------------------------
# Some older PowerShell/.NET defaults negotiate TLS 1.0; GitHub needs TLS 1.2+.
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch {}

New-Item -ItemType Directory -Force -Path $Dir | Out-Null

# Download to a temp file first so a failed/partial download never clobbers an
# existing install.
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("hcc-install-" + [System.IO.Path]::GetRandomFileName() + ".exe")
try {
    $progress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'   # the WebRequest progress bar is very slow
    Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
    $ProgressPreference = $progress
} catch {
    Remove-Item $tmp -Force -ErrorAction SilentlyContinue
    throw "download failed: $($_.Exception.Message)`nCheck that release '$Version' exists and has asset '$asset'."
}

if (-not (Test-Path $tmp) -or (Get-Item $tmp).Length -eq 0) {
    Remove-Item $tmp -Force -ErrorAction SilentlyContinue
    throw "downloaded file is empty - release asset may be missing"
}

Move-Item -Path $tmp -Destination $dest -Force
Ok "installed hcc -> $dest"

# --- PATH (user scope) ------------------------------------------------------------
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries  = if ($userPath) { $userPath -split ';' } else { @() }
if ($entries -notcontains $Dir) {
    $newPath = if ([string]::IsNullOrEmpty($userPath)) { $Dir } else { "$userPath;$Dir" }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    $env:Path = "$env:Path;$Dir"   # also update the current session
    Info "added $Dir to your user PATH (open a new terminal for other apps to see it)"
} else {
    Info "$Dir is already on your PATH"
}

Info "run it: hcc --help"
