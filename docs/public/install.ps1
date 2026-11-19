# Install a checksum-verified native boatparty release. Requires PowerShell 5.1+.
[CmdletBinding()]
param(
    [string] $Version = $env:BOATPARTY_VERSION,
    [string] $InstallDir = $env:BOATPARTY_INSTALL_DIR,
    [ValidateSet('Ask', 'Install', 'Skip')]
    [string] $Prerequisites = $(if ($env:BOATPARTY_PREREQUISITES) { $env:BOATPARTY_PREREQUISITES } else { 'Ask' })
)

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

function Get-BoatpartyDownload([string] $Url, [string] $OutFile) {
    $curl = Get-Command curl.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($curl) {
        $options = @('--disable', '--fail', '--location', '--silent', '--show-error', '--retry', '2',
                     '--proto', '=https', '--proto-redir', '=https')
        if ($OutFile) { $options += @('--output', $OutFile) }
        & $curl.Source @options -- $Url
        if ($LASTEXITCODE) { throw "curl.exe exited $LASTEXITCODE downloading $Url" }
    } elseif ($OutFile) {
        Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $OutFile
    } else {
        (Invoke-WebRequest -UseBasicParsing -Uri $Url).Content
    }
}

$releases = 'https://github.com/amxv/boatparty/releases'
if (-not $Version) {
    $Version = ((Get-BoatpartyDownload 'https://api.github.com/repos/amxv/boatparty/releases/latest') -join "`n" | ConvertFrom-Json).tag_name
}
if ($Version -cnotmatch '^v?[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$') {
    throw "Invalid release version: $Version"
}
$number = $Version.TrimStart('v')
$arch = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$target = switch ($arch) {
    'X64' { 'x86_64-pc-windows-msvc' }
    'Arm64' { 'aarch64-pc-windows-msvc' }
    default { throw "Unsupported Windows architecture: $arch" }
}
if (-not $InstallDir) {
    $InstallDir = Join-Path $env:LOCALAPPDATA 'boatparty\bin'
}
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
[IO.Directory]::CreateDirectory($InstallDir) | Out-Null
$stage = Join-Path $InstallDir ('.install-' + [Guid]::NewGuid())
[IO.Directory]::CreateDirectory($stage) | Out-Null
try {
    $name = "boatparty-$number-$target"
    $archive = "$name.zip"
    $url = "$releases/download/v$number"
    $zip = Join-Path $stage $archive
    $sums = Join-Path $stage 'SHA256SUMS'
    Get-BoatpartyDownload "$url/SHA256SUMS" $sums
    $pattern = '^([0-9a-fA-F]{64})\s+\*?' + [regex]::Escape($archive) + '$'
    $entry = @(Select-String -LiteralPath $sums -Pattern $pattern)
    if ($entry.Count -ne 1) { throw "Missing or ambiguous checksum for $archive" }
    Get-BoatpartyDownload "$url/$archive" $zip
    if ((Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash -ne $entry[0].Matches[0].Groups[1].Value) {
        throw "Checksum mismatch for $archive"
    }
    Expand-Archive -LiteralPath $zip -DestinationPath $stage
    $source = Join-Path $stage "$name\boatparty.exe"
    $actual = & $source --version
    if ($LASTEXITCODE -ne 0 -or $actual -ne "boatparty $number") {
        throw "The downloaded executable did not report boatparty $number"
    }
    $dest = Join-Path $InstallDir 'boatparty.exe'
    if ([IO.File]::Exists($dest)) {
        [IO.File]::Replace($source, $dest, (Join-Path $stage 'previous.exe'))
    } else {
        [IO.File]::Move($source, $dest)
    }
} finally {
    if ([IO.Directory]::Exists($stage)) { [IO.Directory]::Delete($stage, $true) }
}

function Add-BoatpartyPath([string] $Directory) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    if (([Environment]::ExpandEnvironmentVariables("$machinePath;$userPath") -split ';') -notcontains $Directory) {
        $updated = (@($userPath, $Directory) | Where-Object { $_ }) -join ';'
        [Environment]::SetEnvironmentVariable('Path', $updated, 'User')
    }
    if (($env:Path -split ';') -notcontains $Directory) { $env:Path += ";$Directory" }
}

function Find-BoatpartyTools([string[]] $Names, [string[]] $Directories) {
    $missing = @($Names | Where-Object { -not (Get-Command $_ -CommandType Application -ErrorAction SilentlyContinue) })
    if (-not $missing.Count) { return $true }
    foreach ($dir in $Directories) {
        if (-not @($Names | Where-Object { -not [IO.File]::Exists((Join-Path $dir $_)) }).Count) {
            Add-BoatpartyPath $dir
            return $true
        }
    }
    return $false
}

function Install-BoatpartyGit {
    $suffix = if ($arch -eq 'Arm64') { 'arm64' } else { '64-bit' }
    $release = (Get-BoatpartyDownload 'https://api.github.com/repos/git-for-windows/git/releases/latest') -join "`n" | ConvertFrom-Json
    $asset = @($release.assets | Where-Object { $_.name -match "^Git-[0-9.]+-$suffix\.exe$" })
    if ($asset.Count -ne 1 -or $asset[0].digest -notmatch '^sha256:([0-9a-f]{64})$') {
        throw 'Could not identify a checksum-verified Git for Windows installer.'
    }
    $expected = $Matches[1]
    $tempDir = [IO.Directory]::CreateDirectory((Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid())))
    try {
        $setup = Join-Path $tempDir.FullName $asset[0].name
        Get-BoatpartyDownload $asset[0].browser_download_url $setup
        if ((Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash -ne $expected) {
            throw 'Checksum mismatch for Git for Windows.'
        }
        $gitHome = Join-Path $env:LOCALAPPDATA 'Programs\Git'
        $arguments = '/CURRENTUSER /VERYSILENT /NORESTART /NOCLOSEAPPLICATIONS /DIR="{0}"' -f $gitHome
        $process = Start-Process -FilePath $setup -ArgumentList $arguments -Wait -PassThru
        if ($process.ExitCode) { throw "Git for Windows setup exited $($process.ExitCode)." }
    } finally {
        $tempDir.Delete($true)
    }
}

function Install-BoatpartyOpenSSH {
    $powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $arguments = '-NoProfile -NonInteractive -Command "Add-WindowsCapability -Online -Name OpenSSH.Client~~~~0.0.1.0 -ErrorAction Stop | Out-Null"'
    $process = Start-Process -FilePath $powershell -Verb RunAs -ArgumentList $arguments -Wait -PassThru
    if ($process.ExitCode) { throw "Windows OpenSSH Client installation exited $($process.ExitCode)." }
}

Add-BoatpartyPath $InstallDir
Write-Output "Installed boatparty $number in $InstallDir"

if ($Prerequisites -ne 'Skip') {
    $interactive = [Environment]::UserInteractive -and -not [Console]::IsInputRedirected -and
        -not ([Environment]::GetCommandLineArgs() -match '^-NonI(nteractive)?$')
    $gitDirs = @("$env:LOCALAPPDATA\Programs\Git\cmd", "$env:ProgramFiles\Git\cmd")
    $sshDir = Join-Path $env:SystemRoot 'System32\OpenSSH'
    $missingGit = -not (Find-BoatpartyTools @('git.exe') $gitDirs)
    $missingSSH = -not (Find-BoatpartyTools @('ssh.exe', 'ssh-keygen.exe') @($sshDir))
    $missing = @(
        if ($missingGit) { 'Git for Windows (current user)' }
        if ($missingSSH) { 'Windows OpenSSH Client (requires administrator approval)' }
    )
    if ($missing.Count) {
        $names = $missing -join ' and '
        $approved = $Prerequisites -eq 'Install'
        if (-not $approved -and $interactive) {
            $approved = (Read-Host "Install $names now? [y/N]") -match '^y(es)?$'
        }
        if ($approved) {
            if ($missingGit) { Install-BoatpartyGit }
            if ($missingSSH) { Install-BoatpartyOpenSSH }
            if (-not (Find-BoatpartyTools @('git.exe') $gitDirs) -or
                -not (Find-BoatpartyTools @('ssh.exe', 'ssh-keygen.exe') @($sshDir))) {
                throw 'Setup finished but the required tools are still missing. Restart Windows if it requested a reboot, then run boatparty doctor.'
            }
        } else {
            Write-Output "Still needed: $names. To install, set `$env:BOATPARTY_PREREQUISITES = 'Install' and rerun this installer."
        }
    }
    if ($Prerequisites -eq 'Ask' -and $interactive -and
        -not (Find-BoatpartyTools @('boat.exe') @((Join-Path $env:USERPROFILE '.ascii\bin')))) {
        if ((Read-Host 'Install the optional boat CLI and start its sign-in for boatparty login --create-key? [y/N]') -match '^y(es)?$') {
            $boatInstaller = (Get-BoatpartyDownload 'https://boat.dev/install.ps1') -join "`n"
            & ([scriptblock]::Create($boatInstaller))
        }
    }
}
