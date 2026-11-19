param(
    [Parameter(Mandatory)] [string]$Label,
    [int]$Seconds = 300,
    [int]$IntervalMs = 1000,
    [string]$ProcessName = 'warp-oss',
    [string]$OutDir = (Join-Path $PSScriptRoot '..\..\specs\LIGHT-000\measurements')
)

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

function Get-ProcessTree {
    param([int[]]$RootIds)
    $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name
    $ids = [System.Collections.Generic.HashSet[int]]::new()
    $queue = [System.Collections.Generic.Queue[int]]::new()
    foreach ($id in $RootIds) { [void]$ids.Add($id); $queue.Enqueue($id) }
    while ($queue.Count -gt 0) {
        $parent = $queue.Dequeue()
        foreach ($p in $all | Where-Object { $_.ParentProcessId -eq $parent }) {
            if ($ids.Add([int]$p.ProcessId)) { $queue.Enqueue([int]$p.ProcessId) }
        }
    }
    return $ids
}

$roots = @(Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | ForEach-Object Id)
if ($roots.Count -eq 0) { throw "No hay ningún proceso '$ProcessName' en ejecución." }

$logicalCpus = [Environment]::ProcessorCount
$csvPath = Join-Path $OutDir ("{0}_{1:yyyyMMdd_HHmmss}.csv" -f $Label, (Get-Date))
$prevCpu = @{}
$prevTime = Get-Date
$rows = [System.Collections.Generic.List[object]]::new()

$end = (Get-Date).AddSeconds($Seconds)
while ((Get-Date) -lt $end) {
    $ids = Get-ProcessTree -RootIds $roots
    $procs = @(Get-Process -Id ([int[]]$ids) -ErrorAction SilentlyContinue)
    $now = Get-Date
    $elapsed = ($now - $prevTime).TotalSeconds
    $cpuDelta = 0.0
    foreach ($p in $procs) {
        $total = $p.TotalProcessorTime.TotalSeconds
        if ($prevCpu.ContainsKey($p.Id)) { $cpuDelta += $total - $prevCpu[$p.Id] }
        $prevCpu[$p.Id] = $total
    }
    $rows.Add([pscustomobject]@{
        Time         = $now.ToString('HH:mm:ss')
        Processes    = $procs.Count
        WorkingSetMB = [math]::Round((($procs | Measure-Object WorkingSet64 -Sum).Sum) / 1MB, 1)
        PrivateMB    = [math]::Round((($procs | Measure-Object PrivateMemorySize64 -Sum).Sum) / 1MB, 1)
        CpuPercent   = if ($elapsed -gt 0) { [math]::Round(100 * $cpuDelta / ($elapsed * $logicalCpus), 2) } else { 0 }
        Threads      = ($procs | ForEach-Object { $_.Threads.Count } | Measure-Object -Sum).Sum
        Handles      = ($procs | Measure-Object HandleCount -Sum).Sum
    })
    $prevTime = $now
    Start-Sleep -Milliseconds $IntervalMs
}

$rows | Export-Csv -NoTypeInformation -Path $csvPath
$samples = $rows | Select-Object -Skip 1
$summary = [pscustomobject]@{
    Escenario       = $Label
    Muestras        = $samples.Count
    WS_Media_MB     = [math]::Round(($samples | Measure-Object WorkingSetMB -Average).Average, 1)
    WS_Max_MB       = ($samples | Measure-Object WorkingSetMB -Maximum).Maximum
    Privada_Max_MB  = ($samples | Measure-Object PrivateMB -Maximum).Maximum
    CPU_Media_Pct   = [math]::Round(($samples | Measure-Object CpuPercent -Average).Average, 2)
    CPU_Max_Pct     = ($samples | Measure-Object CpuPercent -Maximum).Maximum
    Procesos        = ($samples | Measure-Object Processes -Maximum).Maximum
    Hilos_Max       = ($samples | Measure-Object Threads -Maximum).Maximum
}
$summary | Format-List
Write-Output "CSV: $csvPath"
Write-Output 'Procesos del árbol al final:'
Get-Process -Id ([int[]](Get-ProcessTree -RootIds $roots)) -ErrorAction SilentlyContinue |
    Sort-Object WorkingSet64 -Descending |
    Format-Table Id, ProcessName, @{ n = 'WS_MB'; e = { [math]::Round($_.WorkingSet64 / 1MB, 1) } }, @{ n = 'Priv_MB'; e = { [math]::Round($_.PrivateMemorySize64 / 1MB, 1) } } -AutoSize
