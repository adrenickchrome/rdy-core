# Runs the fast gate on a hosted Windows runner under a temporary standard account.
# The hosted runner is elevated, and an administrator token cannot prove read-only or ACL
# behaviour. This script runs as the runner's administrator. It makes the account, a private
# checkout, and a private Cargo and rustup home. It runs windows_gate_inner.ps1 under the account
# and exits with that script's status. The account's credential exists for this run only: it is
# masked, and it is never written to a file, an argument list or the environment.
$ErrorActionPreference = 'Stop'

$root = 'C:\rdy-ci'
$work = Join-Path $root 'work'
$cargoHome = Join-Path $root 'cargo'
$rustupHome = Join-Path $root 'rustup'
$logs = Join-Path $root 'logs'
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

New-Item -ItemType Directory -Force -Path $root, $cargoHome, $rustupHome, $logs | Out-Null

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

# Tool directories the account needs on its PATH: Node from setup-node, and Git.
$node = Split-Path -Parent (Get-Command node).Source
$git = Split-Path -Parent (Get-Command git).Source
$credential = New-Object System.Management.Automation.PSCredential(".\$account", $secure)

$out = Join-Path $logs 'gate.out'
$err = Join-Path $logs 'gate.err'
$inner = Join-Path $work 'scripts\ci\windows_gate_inner.ps1'
$arguments = @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', (Quote-Arg $inner),
    '-Work', (Quote-Arg $work), '-CargoHome', (Quote-Arg $cargoHome), '-RustupHome', (Quote-Arg $rustupHome),
    '-NodeDir', (Quote-Arg $node), '-GitDir', (Quote-Arg $git))
$process = Start-Process -FilePath 'powershell.exe' -ArgumentList $arguments -Credential $credential `
    -LoadUserProfile -WorkingDirectory $work -RedirectStandardOutput $out -RedirectStandardError $err `
    -Wait -PassThru
$status = $process.ExitCode
$secure.Dispose()

Get-Content -LiteralPath $out
Get-Content -LiteralPath $err | ForEach-Object { [Console]::Error.WriteLine($_) }
Write-Output "Fast gate under $account exited with status $status"
if ($status -ne 0) { exit $status }
exit 0
