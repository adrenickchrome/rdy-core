# Reads the qa report for the Windows fast gate: prints its failing tests and panic excerpts, and
# publishes the pass, fail, ignored and skipped counts to the run page. A missing report is
# said plainly. This script reports and does not decide: the gate step's own status fails the job.
param([Parameter(Mandatory = $true)][string]$Report)
$ErrorActionPreference = 'Stop'

$summary = $env:GITHUB_STEP_SUMMARY
function Publish([string[]]$Lines) {
    if ($summary) { Add-Content -LiteralPath $summary -Value $Lines }
}

if (-not (Test-Path -LiteralPath $Report -PathType Leaf)) {
    Write-Output "No qa report at $Report"
    Publish @('## Windows fast gate', 'No qa report was written.')
    exit 0
}

$lines = Get-Content -LiteralPath $Report
$passed = 0; $failed = 0; $ignored = 0; $skipped = 0
$inAreas = $false
$printing = $false
foreach ($line in $lines) {
    if ($line -like '## *') { $inAreas = ($line -eq '## Areas') }
    # Area rows: | Area | Result | Passed | Failed | Ignored | Skipped |
    if ($inAreas -and $line -match '^\|\s*[^|]+\|\s*[^|]+\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*$') {
        $passed += [int]$Matches[1]; $failed += [int]$Matches[2]
        $ignored += [int]$Matches[3]; $skipped += [int]$Matches[4]
    }
    if ($line -like 'Failing in *') { $printing = $true }
    if ($line -eq '## Perf') { $printing = $false }
    if ($printing) { Write-Output $line }
}

$verdict = ($lines | Where-Object { $_ -like '- verdict:*' } | Select-Object -First 1)
Write-Output "Counts: $passed passed, $failed failed, $ignored ignored, $skipped skipped"
Publish @('## Windows fast gate', $verdict, '',
    '| Passed | Failed | Ignored | Skipped |', '|---|---|---|---|',
    "| $passed | $failed | $ignored | $skipped |")
exit 0
