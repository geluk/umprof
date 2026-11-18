<#
.SYNOPSIS
Records an ETW trace of the running game for a given duration.

.DESCRIPTION
Runs WPR with a CPU profile, capturing traces into the selected output directory.

For convenience, it is recommended to capture traces into the same directory as
MONO_JIT_MAP_DIR, as that will allow umprof.exe to correlate traces and JIT maps
automatically.

Note that you must run this script from an administrator shell.

.EXAMPLE
.\record.ps1 30

.EXAMPLE
.\record.ps1 -Name driving-hb

.LINK
https://github.com/geluk/umprof
#>
param(
    # How long to record, in seconds. If not specified, capturing will continue
    # until prompted to stop.
    [ValidateRange(1, 3600)]
    [int] $Duration,

    # Provide a name to identify this trace. If not specified, an auto-generated
    # name will be used.
    [string] $Name,

    # Directory to write the trace to. This should ideally be the same as
    # MONO_JIT_MAP_DIR.
    [string] $OutputDir = "$pwd",

    # Path to the CPU profile configuration file. If not set, looks for
    # profiles\cpu.wprp next to this script.
    [string] $ProfilePath = (Join-Path $PSScriptRoot "profiles\cpu.wprp")
)

$ErrorActionPreference = "Stop"

$WprProfile = "Cpu"

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "You must run the performance recorder with administrator permissions."
}

New-Item -ItemType Directory -Force $OutputDir | Out-Null

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$fileName = if ($Name) { "trace-$stamp-$Name.etl" } else { "trace-$stamp.etl" }
$filePath = Join-Path $OutputDir $fileName

if (-not (Get-ChildItem $OutputDir -Filter "mono-jit-*.map")) {
    Write-Warning ("No JIT map found in $OutputDir. If this is also your JIT map directory, " +
        "please make sure the game is running with MONO_JIT_MAP_DIR set.")
}

wpr -start "$ProfilePath!$WprProfile" -filemode
if ($LASTEXITCODE -ne 0) {
    throw "WPR failed to start the recording session. If a session was already running, run 'wpr -cancel' to stop it."
}

$stopped = $false
try {
    if ($Duration -eq 0) {
        $start = Get-Date
        $stopRequested = $false
        while (-not $stopRequested) {
            Write-Progress -Activity "Recording $WprProfile" `
                -Status ("{0:hh\:mm\:ss} elapsed, press Q to finish recording" -f ((Get-Date) - $start))
            Start-Sleep -Milliseconds 250
            while ([Console]::KeyAvailable) {
                if ([Console]::ReadKey($true).Key -eq [ConsoleKey]::Q) {
                    $stopRequested = $true
                }
            }
        }
        Write-Progress -Activity "Recording $WprProfile" -Completed
    } else {
        $end = (Get-Date).AddSeconds($Duration)
        while (($left = ($end - (Get-Date)).TotalSeconds) -gt 0) {
            Write-Progress -Activity "Recording $WprProfile" -Status ("{0:0}s left" -f [math]::Ceiling($left)) `
                -PercentComplete (100 * ($Duration - $left) / $Duration)
            Start-Sleep -Milliseconds 250
        }
        Write-Progress -Activity "Recording $WprProfile" -Completed
    }


    Write-Host "Finishing the recording. This may take a moment..."
    wpr -stop $filePath -skipPdbGen
    if ($LASTEXITCODE -ne 0) {
        throw "WPR failed to stop the recording session."
    }
    $stopped = $true
}
finally {
    if (-not $stopped) {
        # Could happen either due to a recording failure or user abort
        wpr -cancel | Out-Null
        Write-Warning "Recording cancelled."
    }
}

Write-Host "Saved $filePath"
Write-Host "To create the profile, run: umprof.exe `"$filePath`""
