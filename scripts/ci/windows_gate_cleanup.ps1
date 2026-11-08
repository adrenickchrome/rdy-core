# Removes the temporary standard account and its profile after the Windows fast gate.
# The account name comes from RDY_CI_ACCOUNT. The script does nothing when the account was never made.
$ErrorActionPreference = 'Stop'

$account = $env:RDY_CI_ACCOUNT
if (-not $account -or $account -notmatch '^rdyci[0-9a-f]{6}$') {
    Write-Output 'No temporary account to remove'
    exit 0
}
$user = Get-LocalUser -Name $account -ErrorAction SilentlyContinue
if ($user) {
    $sid = $user.SID.Value
    Remove-LocalUser -Name $account
    $profile = Get-CimInstance -ClassName Win32_UserProfile -Filter "SID = '$sid'" -ErrorAction SilentlyContinue
    if ($profile) { Remove-CimInstance -InputObject $profile }
}
Write-Output "Removed $account"
exit 0
