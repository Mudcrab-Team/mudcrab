# Mudcrab Installer for Windows
# Usage in PowerShell:
#   irm https://raw.githubusercontent.com/Mudcrab-Team/mudcrab/main/scripts/install.ps1 | iex
#
# "Hey you, you're finally awake."

$ErrorActionPreference = 'Stop'

$Repo = "Mudcrab-Team/mudcrab"
$Target = "x86_64-pc-windows-msvc"

# 1. Architecture Check
if ($env:PROCESSOR_ARCHITECTURE -ne "AMD64" -and $env:PROCESSOR_ARCHITEW6432 -ne "AMD64") {
    Write-Error "Mudcrab currently requires 64-bit Windows (x86_64 / AMD64)."
    exit 1
}

Write-Host "==> Detected Windows 64-bit ($Target)" -ForegroundColor Cyan

# 2. Resolve Version
$Tag = $env:MUDCRAB_VERSION

if (-not $Tag -or $Tag -eq "latest") {
    Write-Host "==> Resolving latest Mudcrab release..." -ForegroundColor Cyan
    try {
        $Request = [System.Net.WebRequest]::Create("https://github.com/$Repo/releases/latest")
        $Request.AllowAutoRedirect = $true
        $Response = $Request.GetResponse()
        $Tag = $Response.ResponseUri.Segments[-1].Trim('/')
        $Response.Close()
    } catch {
        $Tag = "v0.1.0"
    }

    if (-not $Tag -or $Tag -eq "latest") {
        $Tag = "v0.1.0"
        Write-Warning "No published releases found yet on $Repo. Defaulting to $Tag."
    }
} else {
    if (-not $Tag.StartsWith("v")) {
        $Tag = "v$Tag"
    }
}

$ArchiveFileName = "mudcrab-$Tag-$Target.zip"
$DownloadUrl = "https://github.com/$Repo/releases/download/$Tag/$ArchiveFileName"
$ChecksumUrl = "https://github.com/$Repo/releases/download/$Tag/SHA256SUMS.txt"

# 3. Setup Install Directories
$InstallDir = if ($env:MUDCRAB_INSTALL_DIR) { $env:MUDCRAB_INSTALL_DIR } else { "$env:LOCALAPPDATA\Mudcrab" }
$BinDir = "$InstallDir\bin"

if (-not (Test-Path $BinDir)) {
    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
}

$TempDir = [System.IO.Path]::GetTempPath()
$TempZip = Join-Path $TempDir $ArchiveFileName
$TempChecksums = Join-Path $TempDir "SHA256SUMS-$Tag.txt"

# 4. Download Release Archive
Write-Host "==> Downloading Mudcrab $Tag..." -ForegroundColor Cyan
try {
    [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol -bor [System.Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $TempZip -UseBasicParsing
} catch {
    Write-Error "Failed to download Mudcrab from: $DownloadUrl`nVerify that release $Tag exists."
    exit 1
}

# 4.1 Verify Checksum
Write-Host "==> Verifying SHA256 checksum..." -ForegroundColor Cyan
$ChecksumMismatch = $false
$ChecksumError = $null
try {
    # Do not accept a checksum file left behind by a previous installation attempt.
    if (Test-Path $TempChecksums) {
        Remove-Item -Path $TempChecksums -Force
    }
    Invoke-WebRequest -Uri $ChecksumUrl -OutFile $TempChecksums -UseBasicParsing -ErrorAction Stop
    if (Test-Path $TempChecksums) {
        $ExpectedHash = $null
        Get-Content $TempChecksums | ForEach-Object {
            $Parts = $_ -split '\s+'
            if ($Parts.Length -ge 2 -and $Parts[1] -eq $ArchiveFileName) {
                $ExpectedHash = $Parts[0].Trim().ToLower()
            }
        }

        if ($ExpectedHash) {
            $ActualHash = (Get-FileHash -Path $TempZip -Algorithm SHA256).Hash.ToLower()
            if ($ActualHash -ne $ExpectedHash) {
                $ChecksumMismatch = $true
            } else {
                Write-Host "[OK] Checksum verified ($ExpectedHash)" -ForegroundColor Green
            }
        } else {
            throw "No entry for $ArchiveFileName in SHA256SUMS.txt."
        }
    } else {
        throw "SHA256SUMS.txt was not downloaded."
    }
} catch {
    $ChecksumError = "Checksum verification failed: $_"
} finally {
    if (Test-Path $TempChecksums) {
        Remove-Item -Path $TempChecksums -Force -ErrorAction SilentlyContinue
    }
}

if ($ChecksumError) {
    Write-Error $ChecksumError
    exit 1
}

if ($ChecksumMismatch) {
    Write-Error "Checksum verification failed!`nExpected: $ExpectedHash`nGot:      $ActualHash"
    exit 1
}

# 5. Extract Binaries
Write-Host "==> Installing binaries to $BinDir..." -ForegroundColor Cyan
try {
    Expand-Archive -Path $TempZip -DestinationPath $BinDir -Force
} finally {
    if (Test-Path $TempZip) {
        Remove-Item -Path $TempZip -Force -ErrorAction SilentlyContinue
    }
}

# 6. Add to User PATH if missing
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
$NormalizedBin = $BinDir.TrimEnd('\')
$PathEntries = @($UserPath -split ';' | Where-Object { $_ } | ForEach-Object { $_.TrimEnd('\') })
if ($PathEntries -notcontains $NormalizedBin) {
    Write-Host "==> Adding $BinDir to User PATH..." -ForegroundColor Cyan
    [Environment]::SetEnvironmentVariable("Path", "$BinDir;$UserPath", "User")
    $env:Path = "$BinDir;$env:Path"
}

# 7. Create Shortcuts (Desktop & Start Menu)
try {
    $LauncherExe = "$BinDir\launcher.exe"
    if (Test-Path $LauncherExe) {
        $WScriptShell = New-Object -ComObject WScript.Shell

        # Desktop Shortcut
        $DesktopPath = [Environment]::GetFolderPath("Desktop")
        $DesktopShortcut = $WScriptShell.CreateShortcut("$DesktopPath\Mudcrab.lnk")
        $DesktopShortcut.TargetPath = $LauncherExe
        $DesktopShortcut.WorkingDirectory = $InstallDir
        $DesktopShortcut.Description = "Mudcrab - Modern Skyrim Engine & Converter"
        $DesktopShortcut.Save()
        Write-Host "[OK] Created Desktop shortcut" -ForegroundColor Green

        # Start Menu Shortcut
        $StartMenuPath = [Environment]::GetFolderPath("Programs")
        $MudcrabStartFolder = "$StartMenuPath\Mudcrab"
        if (-not (Test-Path $MudcrabStartFolder)) {
            New-Item -ItemType Directory -Path $MudcrabStartFolder -Force | Out-Null
        }
        $StartShortcut = $WScriptShell.CreateShortcut("$MudcrabStartFolder\Mudcrab.lnk")
        $StartShortcut.TargetPath = $LauncherExe
        $StartShortcut.WorkingDirectory = $InstallDir
        $StartShortcut.Description = "Mudcrab - Modern Skyrim Engine & Converter"
        $StartShortcut.Save()
        Write-Host "[OK] Created Start Menu shortcut" -ForegroundColor Green
    }
} catch {
    Write-Warning "Could not create shortcuts: $_"
}

Write-Host ""
Write-Host "Mudcrab $Tag successfully installed!" -ForegroundColor Green
Write-Host "Run 'launcher.exe' or open Mudcrab from your Desktop/Start Menu." -ForegroundColor Yellow
