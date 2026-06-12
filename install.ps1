<#
.SYNOPSIS
    Download and install the `hcc` HolyC compiler on Windows.

.DESCRIPTION
    hcc installs Go-style into a single root directory (HCC_ROOT, default
    %LOCALAPPDATA%\hcc): the compiler at $HCC_ROOT\bin\hcc.exe and the standard
    library at $HCC_ROOT\lib. This script detects your architecture, downloads the
    matching prebuilt binary and the stdlib archive from the GitHub release, lays
    them out under HCC_ROOT, sets the HCC_ROOT user environment variable, and adds
    $HCC_ROOT\bin to your user PATH — just like GOROOT.

    This is the native-Windows installer (PowerShell). On Linux, macOS, or a POSIX
    shell on Windows (Git Bash / MSYS2 / WSL) use install.sh instead.

.PARAMETER Version
    Release tag to install (e.g. v0.1.0). Defaults to the latest release, or the
    HCC_VERSION environment variable if set.

.PARAMETER Root
    Install root (HCC_ROOT). Defaults to %LOCALAPPDATA%\hcc, or the HCC_ROOT
    environment variable if set.

.EXAMPLE
    irm https://raw.githubusercontent.com/adam-soph/solomon/main/install.ps1 | iex

.EXAMPLE
    .\install.ps1 -Version v0.1.0 -Root C:\sdk\hcc
#>
[CmdletBinding()]
param(
    [string]$Version = $(if ($env:HCC_VERSION) { $env:HCC_VERSION } else { 'latest' }),
    [string]$Root    = $(if ($env:HCC_ROOT)    { $env:HCC_ROOT }    else { '' })
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Repo        = 'adam-soph/solomon'
$Bin         = 'hcc'
$StdlibAsset = 'hcc-stdlib.zip'

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

# --- layout under HCC_ROOT --------------------------------------------------------
if ([string]::IsNullOrEmpty($Root)) {
    $Root = Join-Path $env:LOCALAPPDATA 'hcc'
}
$binDir = Join-Path $Root 'bin'
$libDir = Join-Path $Root 'lib'
$dest   = Join-Path $binDir "$Bin.exe"

# --- download URLs ----------------------------------------------------------------
if ($Version -eq 'latest') {
    $base = "https://github.com/$Repo/releases/latest/download"
} else {
    $base = "https://github.com/$Repo/releases/download/$Version"
}
$binUrl    = "$base/$asset"
$stdlibUrl = "$base/$StdlibAsset"

Info "installing hcc ($Version) for windows/$arch"
Info "binary:  $asset"
Info "stdlib:  $StdlibAsset"
Info "root:    $Root"

# Some older PowerShell/.NET defaults negotiate TLS 1.0; GitHub needs TLS 1.2+.
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch {}

New-Item -ItemType Directory -Force -Path $binDir, $libDir | Out-Null

# Stage downloads in a temp dir first, so a failed/partial download never clobbers an
# existing install.
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("hcc-install-" + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$tmpBin    = Join-Path $tmp "$Bin.exe"
$tmpStdlib = Join-Path $tmp $StdlibAsset
try {
    $progress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'   # the WebRequest progress bar is very slow
    Info 'downloading the compiler...'
    Invoke-WebRequest -Uri $binUrl -OutFile $tmpBin -UseBasicParsing
    Info 'downloading the standard library...'
    Invoke-WebRequest -Uri $stdlibUrl -OutFile $tmpStdlib -UseBasicParsing
    $ProgressPreference = $progress
} catch {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    throw "download failed: $($_.Exception.Message)`nCheck that release '$Version' exists and has assets '$asset' and '$StdlibAsset'."
}

if (-not (Test-Path $tmpBin) -or (Get-Item $tmpBin).Length -eq 0) {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    throw "downloaded binary is empty - release asset may be missing"
}

# Install the binary.
Move-Item -Path $tmpBin -Destination $dest -Force

# Install the standard library: replace any previous copy so an upgrade leaves no stale
# modules, then expand the archive into $HCC_ROOT\lib.
Get-ChildItem -Path $libDir -Force -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Expand-Archive -Path $tmpStdlib -DestinationPath $libDir -Force

Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
Ok "installed hcc -> $dest"
Ok "installed stdlib -> $libDir"

# --- environment (user scope): HCC_ROOT + PATH, GOROOT-style ----------------------
[Environment]::SetEnvironmentVariable('HCC_ROOT', $Root, 'User')
$env:HCC_ROOT = $Root   # also for the current session
Info "set HCC_ROOT = $Root (user environment)"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries  = if ($userPath) { $userPath -split ';' } else { @() }
if ($entries -notcontains $binDir) {
    $newPath = if ([string]::IsNullOrEmpty($userPath)) { $binDir } else { "$userPath;$binDir" }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    $env:Path = "$env:Path;$binDir"   # also update the current session
    Info "added $binDir to your user PATH (open a new terminal for other apps to see it)"
} else {
    Info "$binDir is already on your PATH"
}

Info "run it: hcc --help"
