# Install a checksum-verified native boatparty release. Requires PowerShell 5.1+.
[CmdletBinding()]
param(
    [string] $Version = $env:BOATPARTY_VERSION,
    [string] $InstallDir = $env:BOATPARTY_INSTALL_DIR
)

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$releases = 'https://github.com/amxv/boatparty/releases'
if (-not $Version) {
    $Version = (Invoke-RestMethod 'https://api.github.com/repos/amxv/boatparty/releases/latest').tag_name
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
    Invoke-WebRequest -UseBasicParsing "$url/SHA256SUMS" -OutFile $sums
    $pattern = '^([0-9a-fA-F]{64})\s+\*?' + [regex]::Escape($archive) + '$'
    $entry = @(Select-String -LiteralPath $sums -Pattern $pattern)
    if ($entry.Count -ne 1) { throw "Missing or ambiguous checksum for $archive" }
    Invoke-WebRequest -UseBasicParsing "$url/$archive" -OutFile $zip
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

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $InstallDir) {
    $updated = (@($userPath, $InstallDir) | Where-Object { $_ }) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $updated, 'User')
}
if (($env:Path -split ';') -notcontains $InstallDir) { $env:Path += ";$InstallDir" }
Write-Output "Installed boatparty $number in $InstallDir"
