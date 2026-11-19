# Install THOR on Windows, in one command. The assistant runs it in PowerShell
# (the person types and pastes nothing):
#
#   irm https://raw.githubusercontent.com/nworks3d/THOR-for-claude-code/main/install.ps1 | iex
#
# It downloads the latest release, checks the download against the checksum
# published beside it, unpacks it into your home folder, and then runs THOR's
# own setup - which creates the memory and points Claude Code at it.
#
# Two files of yours are touched by that last step, and both are backed up to
# <name>.bak first: Claude Code's settings.json (the hooks) and .claude.json
# (the memory server). Set $env:THOR_NO_SETUP = "1" beforehand to unpack only
# and run the setup afterwards.
#
# SECOND MACHINE: to get a read-only copy of another machine's memory instead
# of a memory of its own, set $env:THOR_COPY_FROM (the backup), and optionally
# $env:THOR_COPY_SCOPES (projects, comma separated) and $env:THOR_COPY_GLOBAL =
# "1", beforehand. For a FULL copy of the whole memory, set $env:THOR_COPY_WHOLE
# = "1" instead of the two (only on a computer you trust, never a work laptop).
# The setup program reads them itself; see "Use THOR on a second machine" in
# README.md.
#
# If git or Microsoft's Visual C++ runtime is missing, winget installs it first,
# or else the official installer does (Windows may ask permission once). THOR itself needs no administrator rights
# and is installed only in your own user folder.

$ErrorActionPreference = 'Stop'
# Without this, Invoke-WebRequest spends most of its time drawing a progress
# bar rather than downloading, which on a file this size is minutes wasted.
$ProgressPreference = 'SilentlyContinue'

$repo = 'nworks3d/THOR-for-claude-code'
$dest = $env:THOR_HOME
if (-not $dest) { $dest = Join-Path $HOME 'thor2' }

$asset = 'thor2-windows-x86_64.zip'
# THOR_RELEASE_BASE points the download at another folder holding the same two
# files (a mirror, a local server, a test); left unset it is the latest release.
$base = $env:THOR_RELEASE_BASE
if (-not $base) { $base = "https://github.com/$repo/releases/latest/download" }
$base = $base.TrimEnd('/')

if ([Environment]::Is64BitOperatingSystem -ne $true) {
    throw "Only 64-bit Windows is published. Build from source for anything else."
}

# Programs THOR needs are installed here first, in this console (no new
# window), by winget, and when winget is missing or does not finish, by the
# program's own official installer, downloaded and run silently by this script.
# Nobody is ever asked to download, paste or run anything. Windows may ask
# permission once per program: clicking Yes is the only part nobody else can do.
function Test-Winget {
    return [bool](Get-Command winget -ErrorAction SilentlyContinue)
}

# A program installed a moment ago is not on the PATH of this console yet:
# read the PATH again from Windows itself.
function Update-SessionPath {
    $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $user = [Environment]::GetEnvironmentVariable('Path', 'User')
    $env:Path = (@($machine, $user) | Where-Object { $_ }) -join ';'
}

function Install-WithWinget([string]$id) {
    & winget install --id $id -e --source winget --silent --accept-package-agreements --accept-source-agreements
    return ($LASTEXITCODE -eq 0)
}

# Download an official installer into a new empty temp folder, run it only if
# its digital signature is valid, wait for it, and delete the folder again.
# Returns $true when it ran and exited with 0 or 3010 (restart pending).
function Install-FromDownload([string]$url, [string]$fileName, [string[]]$installerArgs, [string]$what) {
    $folder = Join-Path ([System.IO.Path]::GetTempPath()) ("thor-dl-" + [System.Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $folder | Out-Null
    try {
        $file = Join-Path $folder $fileName
        try {
            Invoke-WebRequest -Uri $url -OutFile $file -UseBasicParsing
        } catch {
            Write-Host "The $what installer could not be downloaded: $($_.Exception.Message)"
            return $false
        }
        $signature = Get-AuthenticodeSignature -FilePath $file
        if ($signature.Status -ne 'Valid') {
            Write-Host "The downloaded $what installer was NOT run because its digital signature is not valid ($($signature.Status))."
            return $false
        }
        $process = Start-Process -FilePath $file -ArgumentList $installerArgs -Wait -PassThru -WindowStyle Hidden
        return (($process.ExitCode -eq 0) -or ($process.ExitCode -eq 3010))
    } catch {
        Write-Host "The $what installer could not be run: $($_.Exception.Message)"
        return $false
    } finally {
        Remove-Item -Path $folder -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Install-GitFromDownload {
    try {
        $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/git-for-windows/git/releases/latest' -UseBasicParsing -Headers @{ 'User-Agent' = 'thor-install' }
    } catch {
        Write-Host "The latest Git for Windows release could not be looked up: $($_.Exception.Message)"
        return $false
    }
    $assets = @($release.assets | Where-Object { $_.name -match '^Git-[0-9.]+-64-bit\.exe$' })
    if ($assets.Count -eq 0) {
        Write-Host "The latest Git for Windows release has no 64-bit installer."
        return $false
    }
    return (Install-FromDownload $assets[0].browser_download_url $assets[0].name @('/VERYSILENT', '/NORESTART', '/NOCANCEL', '/SP-', '/SUPPRESSMSGBOXES') 'Git for Windows')
}

function Install-RuntimeFromDownload {
    return (Install-FromDownload 'https://aka.ms/vs/17/release/vc_redist.x64.exe' 'vc_redist.x64.exe' @('/install', '/quiet', '/norestart') "Microsoft Visual C++ runtime")
}

function Test-Git {
    return [bool](Get-Command git -ErrorAction SilentlyContinue)
}

# Windows PowerShell 5.1 does not use a modern secure protocol by default.
try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch {}

# git: THOR reads a project's code with it, and Claude Code runs its hooks
# through Git Bash. THOR's own hooks work without it, so a failure here is
# said plainly and the install carries on.
if (-not (Test-Git)) {
    Write-Host "Installing git first: THOR reads a project's code with it, and Claude Code runs its hooks through it."
    if (Test-Winget) {
        [void](Install-WithWinget 'Git.Git')
        Update-SessionPath
    }
    if (-not (Test-Git)) {
        Write-Host "Installing git with its own installer instead."
        [void](Install-GitFromDownload)
        Update-SessionPath
    }
    if (Test-Git) {
        Write-Host "git is installed."
    } else {
        Write-Host "git could not be installed on this computer (winget and the official installer both failed). Carrying on without it: THOR works, but it cannot read a project's code until git can be installed."
    }
}

# The programs need Microsoft's Visual C++ runtime. Without it Windows opens a
# modal error box when a program starts, which nobody can click away from a
# script, so look first. Most machines already have it; if this one does not,
# install it the same way.
$system = [Environment]::GetFolderPath('System')
$runtimeFiles = @('vcruntime140.dll', 'msvcp140.dll')
$missing = @($runtimeFiles | Where-Object { -not (Test-Path (Join-Path $system $_)) })
if ($missing.Count -gt 0) {
    Write-Host "Installing Microsoft's Visual C++ runtime first: THOR's programs cannot start without it."
    if (Test-Winget) {
        [void](Install-WithWinget 'Microsoft.VCRedist.2015+.x64')
    }
    $missing = @($runtimeFiles | Where-Object { -not (Test-Path (Join-Path $system $_)) })
    if ($missing.Count -gt 0) {
        Write-Host "Installing the runtime with its own installer instead."
        [void](Install-RuntimeFromDownload)
        $missing = @($runtimeFiles | Where-Object { -not (Test-Path (Join-Path $system $_)) })
    }
    if ($missing.Count -gt 0) {
        throw ("The Microsoft Visual C++ runtime ($($missing -join ', ')) could not be installed on this computer " +
            "(winget and the official installer both failed), so THOR's programs would not start. Nothing was installed.")
    }
    Write-Host "The Visual C++ runtime is installed."
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("thor-install-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    $zip = Join-Path $tmp $asset
    $sha = "$zip.sha256"

    Write-Host "Downloading THOR (about 90 MB)..."
    try {
        Invoke-WebRequest -Uri "$base/$asset" -OutFile $zip
        Invoke-WebRequest -Uri "$base/$asset.sha256" -OutFile $sha
    } catch {
        throw "THOR could not be downloaded: $($_.Exception.Message). Nothing was installed."
    }

    # The checksum file is "<hash>  <filename>", so only the first field counts.
    $want = ((Get-Content $sha -First 1) -split '\s+')[0]
    $got = (Get-FileHash -Path $zip -Algorithm SHA256).Hash
    if (-not $want) { throw "The checksum file was empty." }
    if ($want -ne $got) {
        throw "The download does not match its published checksum. Expected $want, got $got. Nothing was installed."
    }
    Write-Host "Checksum matches."

    # The archive is flat, so it goes straight into its own directory. An
    # earlier install is overwritten in place; the memory itself lives
    # elsewhere and is never touched by this.
    if (-not (Test-Path $dest)) { New-Item -ItemType Directory -Path $dest | Out-Null }
    Expand-Archive -Path $zip -DestinationPath $dest -Force

    # Files that came out of a downloaded archive are marked as coming from the
    # internet, and Windows then refuses to run them without a warning. Clear
    # that mark on what we just unpacked.
    Get-ChildItem -Path $dest -File | ForEach-Object {
        try { Unblock-File -Path $_.FullName -ErrorAction Stop } catch {}
    }
    Write-Host "Unpacked into $dest"

    if ($env:THOR_NO_SETUP -eq '1') {
        Write-Host ""
        Write-Host "Skipped the setup because THOR_NO_SETUP is 1. The setup program is:"
        Write-Host "  & '$dest\install.exe'"
        return
    }

    Write-Host "Setting up..."
    & (Join-Path $dest 'install.exe')
    # A native program that fails does not stop a script by itself, and a
    # "Done" after it would be a lie.
    if ($LASTEXITCODE -ne 0) {
        throw "The setup did not finish - read the message just above this one. Running the same command again is safe once that is sorted out."
    }

    # The setup above has already printed what it did and the one step left
    # for you. Nothing more is said here: a second, shorter "Done" that
    # disagreed with the first was one more thing a newcomer had to reconcile.
}
finally {
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
