# Prints a disk report to the job log for the Windows fast gate: the size and free space of each
# fixed drive, and the size of the CI root's work, target, home\temp, Cargo home and rustup home
# folders. With -Leftovers it also lists the largest entries left in home\temp, so a test that
# leaks a temporary folder can be named. This script reports and does not decide: it always exits 0.
param(
    [Parameter(Mandatory = $true)][string]$Root,
    [Parameter(Mandatory = $true)][string]$Label,
    # The private home's parent, when it is not the CI root (8.3 fallback in windows_gate.ps1).
    [string]$HomeRoot = $Root,
    [switch]$Leftovers
)
$ErrorActionPreference = 'Continue'

function Format-Gb([double]$Bytes) {
    return ('{0:N1} GB' -f ($Bytes / 1GB))
}

function Get-FolderBytes([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $sum = (Get-ChildItem -LiteralPath $Path -Recurse -Force -File -ErrorAction SilentlyContinue |
        Measure-Object -Property Length -Sum).Sum
    if ($null -eq $sum) { return 0 }
    return [double]$sum
}

Write-Output "Disk report ($Label) at $((Get-Date).ToUniversalTime().ToString('u'))"
foreach ($disk in (Get-CimInstance -ClassName Win32_LogicalDisk -Filter 'DriveType = 3')) {
    Write-Output ("  drive {0} {1}: {2} free of {3}" -f $disk.DeviceID, $disk.FileSystem,
        (Format-Gb $disk.FreeSpace), (Format-Gb $disk.Size))
    # The volume's 8.3 short-name state: tests compare a path's short and long spellings.
    $state = (& fsutil 8dot3name query $disk.DeviceID 2>&1 | Where-Object { $_ -match 'Based on' }) -join ' '
    if ($state) { Write-Output "    $state" }
}
$folders = [ordered]@{
    'target' = (Join-Path $Root 'work\target')
    'home\temp' = (Join-Path $HomeRoot 'home\temp')
    'cargo home' = (Join-Path $Root 'cargo')
    'rustup home' = (Join-Path $Root 'rustup')
}
$workBytes = Get-FolderBytes (Join-Path $Root 'work')
$targetBytes = Get-FolderBytes $folders['target']
if ($null -ne $workBytes) {
    $rest = $workBytes
    if ($null -ne $targetBytes) { $rest -= $targetBytes }
    Write-Output ("  {0}: {1}" -f 'work (without target)', (Format-Gb $rest))
}
foreach ($name in @('target', 'home\temp', 'cargo home', 'rustup home')) {
    $bytes = if ($name -eq 'target') { $targetBytes } else { Get-FolderBytes $folders[$name] }
    if ($null -eq $bytes) { Write-Output "  ${name}: (none)" } else { Write-Output ("  {0}: {1}" -f $name, (Format-Gb $bytes)) }
}

if ($Leftovers) {
    $temp = $folders['home\temp']
    if (Test-Path -LiteralPath $temp) {
        $entries = @(Get-ChildItem -LiteralPath $temp -Force -ErrorAction SilentlyContinue)
        Write-Output "  home\temp holds $($entries.Count) entries; the largest:"
        $sized = foreach ($entry in $entries) {
            $bytes = if ($entry.PSIsContainer) { Get-FolderBytes $entry.FullName } else { [double]$entry.Length }
            [pscustomobject]@{ Name = $entry.Name; Bytes = $bytes }
        }
        foreach ($item in ($sized | Sort-Object -Property Bytes -Descending | Select-Object -First 20)) {
            Write-Output ("    {0}: {1}" -f $item.Name, (Format-Gb $item.Bytes))
        }
        # Grouped by the first four characters of the name (rdys, rdyd, .tmp), to show what leaks.
        $groups = $sized | Group-Object -Property { $_.Name.Substring(0, [Math]::Min(4, $_.Name.Length)) }
        foreach ($group in ($groups | Sort-Object -Property Count -Descending | Select-Object -First 10)) {
            $total = ($group.Group | Measure-Object -Property Bytes -Sum).Sum
            Write-Output ("    prefix {0}: {1} entries, {2}" -f $group.Name, $group.Count, (Format-Gb $total))
        }
    }
}
exit 0
