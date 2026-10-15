# Hourly ship of THOR's memory to a copy, with every failure written down.
#
# WHY THIS SCRIPT RECORDS FAILURES. On 2026-10-11 an antivirus removed sync.exe.
# A task script that simply runs the program with $ErrorActionPreference = 'Stop'
# dies at that line and leaves no trace, so for two days the check said "the
# hourly ship has not run" when it had run and failed. This script catches the
# failure (a missing program included) and writes it beside the memory, in
# sync-ship-state.json, so `doctor` can say what went wrong.
#
# HOW TO USE IT (step by step, nothing here is typed more than once):
#  1. Copy this file next to your THOR programs, for example to
#     C:\Users\<you>\thor2\ship-hourly.ps1
#  2. Set the receiver's shared secret in the environment the task runs in:
#     THOR_SYNC_TOKEN (the ship refuses to run without it).
#  3. Make the hourly task run, hidden (no window):
#     powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass
#       -File "C:\Users\<you>\thor2\ship-hourly.ps1"
#       -SyncExe "C:\Users\<you>\thor2\sync.exe"
#       -Store "C:\Users\<you>\AppData\Local\thor2\thor.db"
#       -To "http://<receiver>:5555"
#  4. Run `doctor` once the task has fired: its "ship:" line shows the last
#     success, or the error of the last failure.
param(
    [Parameter(Mandatory = $true)][string]$SyncExe,
    [Parameter(Mandatory = $true)][string]$Store,
    [Parameter(Mandatory = $true)][string]$To
)

$ErrorActionPreference = 'Stop'

# Write the failure into the sidecar `doctor` reads. The last success on file
# is kept as it is: a failed attempt ships nothing.
function Record-Failure([string]$Why) {
    try {
        $path = Join-Path (Split-Path -Parent $Store) 'sync-ship-state.json'
        $state = [ordered]@{ completed_unix = 0; receiver_seq = 0; covered_seq = $null }
        if (Test-Path -LiteralPath $path) {
            $old = Get-Content -Raw -LiteralPath $path | ConvertFrom-Json
            foreach ($name in @('completed_unix', 'receiver_seq', 'covered_seq')) {
                if ($null -ne $old.$name) { $state[$name] = $old.$name }
            }
        }
        $state['last_attempt_unix'] = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
        $state['last_attempt_error'] = ($Why -replace '\s+', ' ').Trim()
        $json = $state | ConvertTo-Json
        [System.IO.File]::WriteAllText($path, $json, (New-Object System.Text.UTF8Encoding($false)))
    } catch {
        # Nothing more can be done from here; the exit code below still fails the task.
    }
}

try {
    if (-not (Test-Path -LiteralPath $SyncExe)) {
        throw "program not found: $SyncExe (an antivirus may have quarantined it; check Windows Security > Virus & threat protection > Protection history)"
    }
    & $SyncExe ship --db $Store --to $To
    if ($LASTEXITCODE -ne 0) { throw "sync ship exited with code $LASTEXITCODE" }
} catch {
    Record-Failure $_.Exception.Message
    exit 1
}
