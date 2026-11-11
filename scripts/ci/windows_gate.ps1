# Runs the fast gate on a hosted Windows runner under a temporary standard account.
# The hosted runner is elevated, and an administrator token cannot prove read-only or ACL
# behaviour. This script runs as the runner's administrator. It makes the account, a private
# checkout, and a private Cargo and rustup home. It runs windows_gate_inner.ps1 under the account
# and exits with that script's status. The account's credential exists for this run only: it is
# masked, and it is never written to a file, an argument list or the environment.
$ErrorActionPreference = 'Stop'

# The CI root goes on the fixed NTFS drive with the most free space. On run 42959710193 the gate
# filled C: (os error 112 in about 60 tests), while the runner's workspace drive had room. Later
# steps read the same value from RDY_CI_ROOT, so no step names a drive.
$drive = Get-CimInstance -ClassName Win32_LogicalDisk -Filter 'DriveType = 3' |
    Where-Object { $_.FileSystem -eq 'NTFS' } | Sort-Object -Property FreeSpace -Descending |
    Select-Object -First 1
if (-not $drive) { throw 'No fixed NTFS drive for the CI root' }
$root = Join-Path ($drive.DeviceID + '\') 'rdy-ci'
Add-Content -Path $env:GITHUB_ENV -Value "RDY_CI_ROOT=$root"
Write-Output "CI root: $root"
$diskReport = Join-Path $PSScriptRoot 'windows_disk_report.ps1'
# A failed report is printed and never stops the gate.
function Write-DiskReport {
    try { & $diskReport @args } catch { Write-Output "Disk report failed: $_" }
}
$work = Join-Path $root 'work'
$cargoHome = Join-Path $root 'cargo'
$rustupHome = Join-Path $root 'rustup'
$logs = Join-Path $root 'logs'
$home_ = Join-Path $root 'home'
$channel = '1.99.0'

function Invoke-Native {
    param([string]$File, [string[]]$Arguments)
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$File failed with status $LASTEXITCODE" }
}

# Start-Process joins ArgumentList elements with spaces and does not quote them, so a path such as
# C:\Program Files\Git would split. Each path value goes in double quotes.
function Quote-Arg {
    param([string]$Value)
    return '"' + $Value + '"'
}

$homeDirs = @((Join-Path $home_ 'temp'), (Join-Path $home_ 'appdata'), (Join-Path $home_ 'localappdata'))
New-Item -ItemType Directory -Force -Path (@($root, $cargoHome, $rustupHome, $logs) + $homeDirs) | Out-Null

# The credential: random bytes, masked before first use, held in memory only.
$bytes = New-Object byte[] 24
$random = [System.Security.Cryptography.RandomNumberGenerator]::Create()
$random.GetBytes($bytes)
$random.Dispose()
$password = 'Aa1!' + [Convert]::ToBase64String($bytes)
Write-Output "::add-mask::$password"
$secure = ConvertTo-SecureString -String $password -AsPlainText -Force
$password = $null

$suffix = -join ((1..6) | ForEach-Object { '{0:x}' -f (Get-Random -Maximum 16) })
$account = "rdyci$suffix"
New-LocalUser -Name $account -Password $secure -AccountNeverExpires -PasswordNeverExpires `
    -Description 'Temporary standard account for one CI run' | Out-Null
Add-Content -Path $env:GITHUB_ENV -Value "RDY_CI_ACCOUNT=$account"
Add-LocalGroupMember -Group 'Users' -Member $account

# The root's ACL does not depend on the drive it is on: only the account, the administrators and
# SYSTEM can open it, and the folders under it inherit that. Same order as for the private home
# below: the explicit, inheritable grants first, then the inherited entries go.
Invoke-Native 'icacls' @($root, '/grant:r', "${account}:(OI)(CI)F", '*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', '/Q')
Invoke-Native 'icacls' @($root, '/inheritance:r', '/Q')
Invoke-Native 'icacls' @($root)
Write-DiskReport -Root $root -Label 'start'

# The toolchain goes into the private homes as the administrator. The account then owns them.
$env:CARGO_HOME = $cargoHome
$env:RUSTUP_HOME = $rustupHome
Invoke-Native 'rustup' @('toolchain', 'install', $channel, '--profile', 'minimal', '--component', 'clippy,rustfmt')
$proxies = Join-Path $cargoHome 'bin'
New-Item -ItemType Directory -Force -Path $proxies | Out-Null
Copy-Item -Path (Join-Path $env:USERPROFILE '.cargo\bin\*.exe') -Destination $proxies -Force

# The private checkout. Robocopy reports success with a status below 8.
robocopy $env:GITHUB_WORKSPACE $work /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
if ($LASTEXITCODE -ge 8) { throw "robocopy failed with status $LASTEXITCODE" }

foreach ($dir in @($work, $cargoHome, $rustupHome, $logs)) {
    Invoke-Native 'icacls' @($dir, '/setowner', $account, '/T', '/C', '/Q')
    Invoke-Native 'icacls' @($dir, '/grant', "${account}:(OI)(CI)F", '/T', '/C', '/Q')
}

# The account's own TEMP, TMP, APPDATA and LOCALAPPDATA live here, private to the account. Without
# them the child inherits the runner account's paths, which the standard account cannot write
# (the linker fails with LNK1104). Only the account, the administrators and SYSTEM can open this
# folder. The order matters: removing inheritance first leaves an empty DACL, and once the account
# owns the folder the administrator can no longer change it ("Access is denied"). So the explicit,
# inheritable grants come first, while the administrator still owns the folder; then the inherited
# entries go; then the account takes ownership. No /C: icacls with /C exits 0 after a failure.
Invoke-Native 'icacls' @($home_, '/grant:r', "${account}:(OI)(CI)F", '*S-1-5-32-544:(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', '/T', '/Q')
Invoke-Native 'icacls' @($home_, '/inheritance:r', '/Q')
Invoke-Native 'icacls' @($home_, '/setowner', $account, '/T', '/Q')
# The ACL in the log, so a later failure can be read without a rerun.
Invoke-Native 'icacls' @($home_)
Invoke-Native 'icacls' @((Join-Path $home_ 'temp'))

# Tool directories the account needs on its PATH: Node from setup-node, and Git.
$node = Split-Path -Parent (Get-Command node).Source
$git = Split-Path -Parent (Get-Command git).Source
$credential = New-Object System.Management.Automation.PSCredential(".\$account", $secure)

$out = Join-Path $logs 'gate.out'
$err = Join-Path $logs 'gate.err'
$inner = Join-Path $work 'scripts\ci\windows_gate_inner.ps1'
$arguments = @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', (Quote-Arg $inner),
    '-Work', (Quote-Arg $work), '-PrivateHome', (Quote-Arg $home_), '-CargoHome', (Quote-Arg $cargoHome), '-RustupHome', (Quote-Arg $rustupHome),
    '-NodeDir', (Quote-Arg $node), '-GitDir', (Quote-Arg $git))
# The account starts with a fresh environment: pass the hosted-runner mark on explicitly.
if ($env:RDY_QA_HOSTED_CI -eq '1') { $arguments += '-HostedCi' }
$process = Start-Process -FilePath 'powershell.exe' -ArgumentList $arguments -Credential $credential `
    -LoadUserProfile -WorkingDirectory $work -RedirectStandardOutput $out -RedirectStandardError $err `
    -PassThru
# Reading the handle keeps ExitCode available after the process exits.
$null = $process.Handle
# A disk report every 15 minutes while the gate runs (build, then tests), and one at the end.
while (-not $process.WaitForExit(900000)) {
    Write-DiskReport -Root $root -Label 'during the gate'
}
$process.WaitForExit()
$status = $process.ExitCode
Write-DiskReport -Root $root -Label 'end' -Leftovers
$secure.Dispose()

Get-Content -LiteralPath $out
Get-Content -LiteralPath $err | ForEach-Object { [Console]::Error.WriteLine($_) }
Write-Output "Fast gate under $account exited with status $status"
if ($status -ne 0) { exit $status }
exit 0
