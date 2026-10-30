<#
Read-only bounded Capsule process-tree sampler (Windows PowerShell 5.1 / PowerShell 7).
Example: .\scripts\performance-measure.ps1 -RootProcessId 1234 -Label idle -Seconds 10
Use the Capsule executable PID, not cargo/tauri/vite. Repeat with Label playback
while playback is already active. This script never launches/stops the app or
changes its settings. CIM process enumeration may require an elevated terminal.
Private bytes are committed private memory; working set includes shared pages,
so summed working set can double count shared physical memory. CPU one-core %
can exceed 100; machine % divides by the machine logical processor count.
CSV reports observed snapshots, not startup/transition/audio latency. Short-lived
processes between snapshots cannot be observed; churn/missing reads flag CPU
intervals incomplete instead of silently presenting them as full accounting.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateRange(1, 2147483647)][int]$RootProcessId,
    [ValidateRange(1, 300)][int]$Seconds = 10,
    [ValidateRange(200, 10000)][int]$IntervalMs = 1000,
    [ValidatePattern('^[a-zA-Z0-9_-]+$')][string]$Label = 'baseline',
    [string]$OutputDirectory = (Join-Path $env:TEMP 'capsule-performance')
)
$ErrorActionPreference = 'Stop'
$logicalProcessors = [Environment]::ProcessorCount
$initial = Get-CimInstance Win32_Process -Filter "ProcessId = $RootProcessId"
if (-not $initial) { throw "Root PID $RootProcessId is not running." }
$rootCreation = $initial.CreationDate.ToUniversalTime().Ticks
$rootStarted = $initial.CreationDate.ToUniversalTime().ToString('o')
$sampleRows = [System.Collections.Generic.List[object]]::new()
$processRows = [System.Collections.Generic.List[object]]::new()
$previous = @{}
$previousUtc = $null
$previousElapsed = 0.0
$clock = [Diagnostics.Stopwatch]::StartNew()
$sample = 0
do {
    $snapshot = @(Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,CreationDate,Name,CommandLine)
    $root = $snapshot | Where-Object { [int]$_.ProcessId -eq $RootProcessId }
    if (-not $root -or $root.CreationDate.ToUniversalTime().Ticks -ne $rootCreation) {
        Write-Warning 'Root exited or its PID was reused; stopping at the last valid sample.'
        break
    }
    $tree = @{}; $tree[$RootProcessId] = $root
    do {
        $added = $false
        foreach ($candidate in $snapshot) {
            $id = [int]$candidate.ProcessId
            $parentId = [int]$candidate.ParentProcessId
            if (-not $tree.ContainsKey($id) -and $tree.ContainsKey($parentId) -and
                $candidate.CreationDate -ge $tree[$parentId].CreationDate) {
                $tree[$id] = $candidate; $added = $true
            }
        }
    } while ($added)
    $current = @{}; $missing = 0; $privateBytes = 0L; $workingSet = 0L
    $utc = [DateTime]::UtcNow
    foreach ($entry in $tree.Values) {
        try {
            $process = Get-Process -Id ([int]$entry.ProcessId) -ErrorAction Stop
            $process.Refresh()
            $started = $process.StartTime.ToUniversalTime()
            # CIM retains microseconds; Get-Process obtains the same OS creation time.
            if ([Math]::Abs(($started - $entry.CreationDate.ToUniversalTime()).TotalMilliseconds) -gt 1) {
                throw 'PID creation-time mismatch'
            }
            $key = '{0}:{1}' -f $entry.ProcessId, $started.Ticks
            $cpu = $process.TotalProcessorTime.TotalSeconds
            $private = $process.PrivateMemorySize64
            $working = $process.WorkingSet64
            if ($process.HasExited) { throw 'Process exited during read' }
            $current[$key] = @{ Cpu = $cpu; Started = $started }
            $privateBytes += $private; $workingSet += $working
            $processRows.Add([pscustomobject]@{
                Sample = $sample; TimestampUtc = $utc.ToString('o'); PID = $entry.ProcessId
                ParentPID = $entry.ParentProcessId; Name = $entry.Name
                StartedUtc = $started.ToString('o'); PrivateBytes = $private
                WorkingSetBytes = $working; CumulativeCpuSeconds = $cpu
                CommandLine = $entry.CommandLine
            })
        } catch { $missing++ }
    }
    $elapsed = $clock.Elapsed.TotalSeconds
    $deltaCpu = 0.0; $exited = 0; $unmatched = 0
    if ($previousUtc) {
        foreach ($key in $current.Keys) {
            if ($previous.ContainsKey($key)) { $deltaCpu += [Math]::Max(0.0, [double]($current[$key].Cpu - $previous[$key].Cpu)) }
            elseif ($current[$key].Started -ge $previousUtc) { $deltaCpu += $current[$key].Cpu }
            else { $unmatched++ }
        }
        foreach ($key in $previous.Keys) { if (-not $current.ContainsKey($key)) { $exited++ } }
    }
    $wall = $elapsed - $previousElapsed
    $oneCore = if ($previousUtc) { 100 * $deltaCpu / $wall } else { $null }
    $sampleRows.Add([pscustomobject]@{
        Sample = $sample; Label = $Label; TimestampUtc = $utc.ToString('o')
        ElapsedSeconds = $elapsed; IntervalSeconds = $wall
        RootPID = $RootProcessId; RootStartedUtc = $rootStarted
        LogicalProcessors = $logicalProcessors; ProcessCount = $current.Count
        MissingProcessReads = $missing; ExitedSincePriorSample = $exited
        UnmatchedExistingProcesses = $unmatched
        CpuIntervalComplete = [bool]($previousUtc -and $missing -eq 0 -and $exited -eq 0 -and $unmatched -eq 0 -and $previousMissing -eq 0)
        PrivateBytes = $privateBytes; WorkingSetBytes = $workingSet
        CpuDeltaSeconds = $(if ($previousUtc) { $deltaCpu } else { $null })
        CpuOneCorePercent = $oneCore
        CpuMachinePercent = $(if ($previousUtc) { $oneCore / $logicalProcessors } else { $null })
    })
    $previous = $current; $previousUtc = $utc; $previousElapsed = $elapsed; $previousMissing = $missing
    $sample++
    if ($elapsed -ge $Seconds) { break }
    Start-Sleep -Milliseconds ([Math]::Min($IntervalMs, [Math]::Max(1, [int](1000 * ($Seconds - $elapsed)))))
} while ($true)
if ($sampleRows.Count -eq 0) { throw 'No valid samples were collected.' }
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$stem = Join-Path $OutputDirectory ('{0}-{1}-{2}' -f $Label, $RootProcessId, (Get-Date -Format 'yyyyMMdd-HHmmss'))
$sampleRows | Export-Csv "$stem-samples.csv" -NoTypeInformation
$processRows | Export-Csv "$stem-processes.csv" -NoTypeInformation
$complete = @($sampleRows | Where-Object CpuIntervalComplete)
$totalWall = ($complete | Measure-Object IntervalSeconds -Sum).Sum
$totalCpu = ($complete | Measure-Object CpuDeltaSeconds -Sum).Sum
[pscustomobject]@{
    Label = $Label; RootPID = $RootProcessId; RootStartedUtc = $rootStarted
    Samples = $sampleRows.Count; DurationSeconds = $clock.Elapsed.TotalSeconds
    LogicalProcessors = $logicalProcessors
    ProcessCountMin = ($sampleRows | Measure-Object ProcessCount -Minimum).Minimum
    ProcessCountMax = ($sampleRows | Measure-Object ProcessCount -Maximum).Maximum
    PrivateMiBMean = ($sampleRows | Measure-Object PrivateBytes -Average).Average / 1MB
    PrivateMiBMax = ($sampleRows | Measure-Object PrivateBytes -Maximum).Maximum / 1MB
    WorkingSetMiBMean = ($sampleRows | Measure-Object WorkingSetBytes -Average).Average / 1MB
    WorkingSetMiBMax = ($sampleRows | Measure-Object WorkingSetBytes -Maximum).Maximum / 1MB
    CompleteCpuIntervals = $complete.Count
    CpuOneCorePercent = $(if ($totalWall -gt 0) { 100 * $totalCpu / $totalWall } else { $null })
    CpuMachinePercent = $(if ($totalWall -gt 0) { 100 * $totalCpu / $totalWall / $logicalProcessors } else { $null })
    SamplesCsv = "$stem-samples.csv"; ProcessesCsv = "$stem-processes.csv"
} | ConvertTo-Json
