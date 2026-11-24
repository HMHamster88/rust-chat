# Measure one process tree on Windows.
#
# Usage:
#   .\scripts\measure.ps1 -Exe .\spikes\ferroui\target\debug\ferroui-spike.exe
#   .\scripts\measure.ps1 -Exe .\target\debug\rust-chat.exe -Seconds 20
#   .\scripts\measure.ps1 -Name rust-chat
#
# Reports peak and median working set / private bytes for the tree, and CPU as a
# percentage of one core over the sampling window. The app is stopped at the end
# unless -KeepRunning is passed.

param(
    [string]$Exe,
    [string]$Name,
    [string[]]$Arguments = @(),
    [int]$WarmupSeconds = 5,
    [int]$Seconds = 10,
    [int]$IntervalMs = 1000,
    [switch]$KeepRunning
)

$ErrorActionPreference = "Stop"

function Get-Tree([int]$RootId) {
    $all = Get-CimInstance Win32_Process
    $ids = @($RootId)
    $changed = $true
    while ($changed) {
        $changed = $false
        foreach ($p in $all) {
            if ($ids -contains [int]$p.ParentProcessId -and -not ($ids -contains [int]$p.ProcessId)) {
                $ids += [int]$p.ProcessId
                $changed = $true
            }
        }
    }
    $ids
}

if ($Exe) {
    $start = Start-Process -FilePath $Exe -ArgumentList $Arguments -PassThru
    $rootId = $start.Id
    "started $Exe (pid $rootId), warming up ${WarmupSeconds}s"
} elseif ($Name) {
    $match = Get-Process -Name $Name -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $match) { throw "no process named '$Name' is running" }
    $rootId = $match.Id
    "attached to $Name (pid $rootId)"
} else {
    throw "pass -Exe or -Name"
}

Start-Sleep -Seconds $WarmupSeconds

$samples = @()
$previousCpu = $null
$previousAt = $null

for ($i = 0; $i -lt [Math]::Max(1, $Seconds * 1000 / $IntervalMs); $i++) {
    $ids = Get-Tree $rootId
    $working = 0L
    $private = 0L
    $threads = 0
    $cpu = 0.0
    foreach ($id in $ids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p) {
            $working += $p.WorkingSet64
            $private += $p.PrivateMemorySize64
            $threads += $p.Threads.Count
            try { $cpu += $p.TotalProcessorTime.TotalSeconds } catch {}
        }
    }
    $now = Get-Date
    $cpuPercent = $null
    if ($previousCpu -ne $null) {
        $elapsed = ($now - $previousAt).TotalSeconds
        if ($elapsed -gt 0) { $cpuPercent = ($cpu - $previousCpu) / $elapsed * 100.0 }
    }
    $previousCpu = $cpu
    $previousAt = $now

    $samples += [pscustomobject]@{
        Second  = $i * $IntervalMs / 1000
        Procs   = $ids.Count
        Working = [Math]::Round($working / 1MB, 1)
        Private = [Math]::Round($private / 1MB, 1)
        Threads = $threads
        CpuOne  = if ($cpuPercent -ne $null) { [Math]::Round($cpuPercent, 1) } else { $null }
    }
    Start-Sleep -Milliseconds $IntervalMs
}

$samples | Format-Table -AutoSize

$workingValues = $samples.Working | Where-Object { $_ -ne $null }
$privateValues = $samples.Private | Where-Object { $_ -ne $null }
$cpuValues = $samples.CpuOne | Where-Object { $_ -ne $null }
if ($workingValues.Count -gt 0) {
    $sorted = $workingValues | Sort-Object
    "working set: peak {0} MB, median {1} MB" -f ($workingValues | Measure-Object -Maximum).Maximum, $sorted[[int]($sorted.Count / 2)]
    $sorted = $privateValues | Sort-Object
    "private bytes: peak {0} MB, median {1} MB" -f ($privateValues | Measure-Object -Maximum).Maximum, $sorted[[int]($sorted.Count / 2)]
}
if ($cpuValues.Count -gt 0) {
    "cpu: mean {0:N1}% of one core, peak {1:N1}%" -f ($cpuValues | Measure-Object -Average).Average, ($cpuValues | Measure-Object -Maximum).Maximum
}

if (-not $KeepRunning) {
    foreach ($id in (Get-Tree $rootId)) {
        Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
    }
    "stopped the process tree"
}
