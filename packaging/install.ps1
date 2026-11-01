# Install watt-start from the latest GitHub release into this user account.
# No administrator rights. From PowerShell:
#
#   irm https://github.com/gusahlg/watt-start/releases/latest/download/install.ps1 | iex
#
# The binary lands in %LOCALAPPDATA%\Programs\watt-start and a Start-menu
# shortcut is created. This does not install Git, Rust or the Visual Studio
# Build Tools the game itself needs in order to compile.

$ErrorActionPreference = "Stop"
# Windows PowerShell 5.1 defaults to TLS 1.0. GitHub requires TLS 1.2.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

function Write-Info([string]$Message) {
    Write-Host $Message
}

if ($env:OS -ne "Windows_NT") {
    throw "install.ps1 is for Windows."
}

$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne "AMD64" -and $env:PROCESSOR_ARCHITEW6432 -eq "AMD64") {
    # 32-bit PowerShell on 64-bit Windows.
    $arch = "AMD64"
}
if ($arch -ne "AMD64") {
    throw "No watt-start build for this CPU ($arch). Releases ship x86_64-windows-msvc."
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Warning "This installer does not need an administrator. It installs into this account's LOCALAPPDATA, not Program Files."
}

$headers = @{
    "User-Agent" = "watt-start-install"
    "Accept"     = "application/vnd.github+json"
}
Write-Info "Looking up the latest release..."
$rel = Invoke-RestMethod -Headers $headers -UseBasicParsing -Uri "https://api.github.com/repos/gusahlg/watt-start/releases/latest"
$tag = [string]$rel.tag_name
if ([string]::IsNullOrWhiteSpace($tag)) {
    throw "Could not read the latest release tag."
}
$version = $tag
if ($version.StartsWith("v")) {
    $version = $version.Substring(1)
}

$zipName = "watt-start-$version-x86_64-windows-msvc.zip"
$url = "https://github.com/gusahlg/watt-start/releases/download/$tag/$zipName"
$dest = Join-Path $env:LOCALAPPDATA "Programs\watt-start"
if (-not (Test-Path $dest)) {
    New-Item -ItemType Directory -Path $dest | Out-Null
}
$exe = Join-Path $dest "watt-start.exe"

$zip = Join-Path $env:TEMP "watt-start-install.zip"
$unpack = Join-Path $env:TEMP ("watt-start-install-" + [guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $unpack | Out-Null
try {
    Write-Info "Downloading $url"
    # Invoke-WebRequest's progress bar makes this very slow on 5.1.
    $ProgressPreference = "SilentlyContinue"
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    Expand-Archive -Path $zip -DestinationPath $unpack
    $found = Get-ChildItem -Path $unpack -Filter "watt-start.exe" -Recurse | Select-Object -First 1
    if (-not $found) {
        throw "The archive does not contain watt-start.exe."
    }
    # Replace only after a complete download, so a failed run leaves the
    # previous install working.
    if (Test-Path $exe) {
        try {
            Remove-Item $exe -Force -ErrorAction Stop
        } catch {
            throw "Close watt-start and run the installer again ($exe is in use)."
        }
    }
    Copy-Item $found.FullName $exe -Force
    Unblock-File -Path $exe
} finally {
    if (Test-Path $unpack) {
        Remove-Item $unpack -Recurse -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path $zip) {
        Remove-Item $zip -Force -ErrorAction SilentlyContinue
    }
}

$programs = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
if (-not (Test-Path $programs)) {
    New-Item -ItemType Directory -Path $programs | Out-Null
}
$shortcutPath = Join-Path $programs "watt-start.lnk"
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $exe
$shortcut.WorkingDirectory = $dest
$shortcut.Description = "Play, update and extend Project Watt Cubed with modules"
$shortcut.Save()

Write-Info ""
Write-Info "Installed watt-start $version"
Write-Info "  program   $exe"
Write-Info "  shortcut  $shortcutPath"
Write-Info ""
Write-Info "Start watt-start from the Start menu."
Write-Info "The first game build compiles Project Watt Cubed and takes 15-40 minutes."
Write-Info "That build needs Git for Windows, rustup with the MSVC toolchain,"
Write-Info "Visual Studio Build Tools (Desktop development with C++), and a Vulkan driver."
Write-Info "To run watt-start from a terminal, add this directory to your user PATH:"
Write-Info "  $dest"
