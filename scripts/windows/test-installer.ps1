# Run on a disposable Windows runner. Exercise a fresh current-user Git
# install without uninstalling the copy Actions uses for its checkout.
$ErrorActionPreference = 'Stop'
$installer = Resolve-Path "$PSScriptRoot\..\..\docs\public\install.ps1"
$testRoot = Join-Path $env:RUNNER_TEMP 'prerequisite test'
$bin = Join-Path $testRoot 'bin'
$env:LOCALAPPDATA = Join-Path $testRoot 'user'
$env:ProgramFiles = Join-Path $testRoot 'programs'
$env:Path = "$env:SystemRoot\System32;$env:SystemRoot\System32\OpenSSH;$env:SystemRoot\System32\WindowsPowerShell\v1.0"
if (Get-Command git.exe -ErrorAction SilentlyContinue) { throw 'Git should be absent at the start' }

# The CI PowerShell is noninteractive. An ordinary run must report a
# missing prerequisite and complete without installing other applications.
$messages = & $installer -Version v0.2.1 -InstallDir $bin -Prerequisites Ask
if ("$messages" -notmatch 'Still needed: Git for Windows') { throw "Missing Git prompt guidance: $messages" }
if (Get-Command git.exe -ErrorAction SilentlyContinue) { throw 'Git installed without consent' }

. $installer -Version v0.2.1 -InstallDir $bin -Prerequisites Install
$expected = Join-Path $env:LOCALAPPDATA 'Programs\Git\cmd\git.exe'
if ((Get-Command git.exe).Source -ne $expected) { throw 'New Git installation is missing from PATH' }
git --version
if ($LASTEXITCODE) { throw 'New Git installation failed to start' }
Install-BoatpartyOpenSSH
if (-not (Find-BoatpartyTools @('ssh.exe', 'ssh-keygen.exe') @("$env:SystemRoot\System32\OpenSSH"))) {
    throw 'Windows OpenSSH client feature failed to install'
}

# Upgrade again with tools now present.
& $installer -Version v0.2.1 -InstallDir $bin -Prerequisites Install
boatparty --version
