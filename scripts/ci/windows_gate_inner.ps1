# The fast gate, run by windows_gate.ps1 under a temporary standard account. It refuses to run
# elevated, uses only the private homes it is given, and exits with the first failing status.
param(
    [Parameter(Mandatory = $true)][string]$Work,
    [Parameter(Mandatory = $true)][string]$CargoHome,
    [Parameter(Mandatory = $true)][string]$RustupHome,
    [Parameter(Mandatory = $true)][string]$NodeDir,
    [Parameter(Mandatory = $true)][string]$GitDir,
    [Parameter(Mandatory = $true)][string]$PrivateHome
)
$ErrorActionPreference = 'Stop'

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Output 'The fast gate must not run as an administrator'
    exit 1
}

# The runner account's TEMP, TMP, APPDATA and LOCALAPPDATA are not writable here. Use private ones.
$env:TEMP = Join-Path $PrivateHome 'temp'
$env:TMP = $env:TEMP
$env:APPDATA = Join-Path $PrivateHome 'appdata'
$env:LOCALAPPDATA = Join-Path $PrivateHome 'localappdata'
# The linker writes lnk*.tmp to TMP. Prove the account can create and delete a file there first.
$probe = Join-Path $env:TMP ('rdy-probe-' + [guid]::NewGuid().ToString('N') + '.tmp')
try {
    New-Item -ItemType File -Path $probe -Force | Out-Null
    Remove-Item -LiteralPath $probe -Force
} catch {
    Write-Output "The account cannot create and delete a file in TMP ($env:TMP): $_"
    exit 1
}
Write-Output "TMP is writable: $env:TMP"
$env:CARGO_HOME = $CargoHome
$env:RUSTUP_HOME = $RustupHome
$env:PATH = (Join-Path $CargoHome 'bin') + ';' + $NodeDir + ';' + $GitDir + ';' + $env:PATH
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$env:CARGO_INCREMENTAL = '0'
Set-Location -LiteralPath $Work

cargo fetch --locked
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Offline after the locked fetch: the gate builds only what Cargo.lock names.
$env:CARGO_NET_OFFLINE = 'true'
cargo qa --skip-perf --require-all
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
exit 0
