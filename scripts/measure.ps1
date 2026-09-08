<#
.SYNOPSIS
    Инструмент замера производительности и ресурсов процесса resticker под требования SPEC.md §13.

.DESCRIPTION
    Скрипт выполняет мониторинг живого процесса resticker.exe в режиме только чтения:
    - Семплирует CPU % (по дельте TotalProcessorTime, нормируя на число логических ядер);
    - Семплирует Working Set (физическая память), Private Bytes / Commit (приватная выделенная память)
      и Working Set Private (WMI PerfProc);
    - Семплирует число потоков, хендлов, GDI- и User-объектов (через Win32 GetGuiResources);
    - Потоково записывает метрики в CSV без накопления строк в памяти (безопасно для 24-часовых прогонов);
    - Формирует итоговую сводку (Min / Mean / Max) и выносит вердикт против порогов SPEC §13;
    - При ключе -ColdStart вычисляет время холодного старта по таймстемпу процесса и маркерам в логе.

    Дата создания: 2026-09-07.
    Скрипт строго пассивен: не перезапускает процесс и не меняет конфигурационные файлы.

.PARAMETER IntervalSec
    Интервал между замерами в секундах (по умолчанию 5).

.PARAMETER DurationSec
    Общая длительность замера в секундах (по умолчанию 600 — 10 минут).

.PARAMETER CsvPath
    Путь для сохранения CSV-отчёта. По умолчанию пишется во временный каталог пользователя.

.PARAMETER ProcessName
    Имя процесса (по умолчанию 'resticker').

.PARAMETER ColdStart
    Ключ для замера времени холодного старта (от запуска процесса до создания первого оверлея по логам).
#>

[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [double]$IntervalSec = 5.0,

    [Parameter(Position = 1)]
    [int]$DurationSec = 600,

    [Parameter(Position = 2)]
    [string]$CsvPath = "",

    [Parameter()]
    [string]$ProcessName = "resticker",

    [Parameter()]
    [switch]$ColdStart
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# --- 1. Подключение Win32 API для замера GDI и User объектов ---
# GetGuiResources позволяет отслеживать утечки оконных хендлов и контекстов рисования,
# критичные для длительных Windows-сессий с оверлеями.
if (-not ([System.Management.Automation.PSTypeName]'Win32GuiMetrics').Type) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class Win32GuiMetrics {
    [DllImport("user32.dll", SetLastError = true)]
    public static extern uint GetGuiResources(IntPtr hProcess, uint uiFlags);
}
"@
}

# --- 2. Поиск целевого процесса ---
# По правилам безопасности: если процесс не найден или их несколько — выходим без догадок.
$processes = @(Get-Process -Name $ProcessName -ErrorAction SilentlyContinue)

if ($processes.Count -eq 0) {
    Write-Error "Процесс '$ProcessName' не найден. Убедитесь, что приложение запущено."
    return
}

if ($processes.Count -gt 1) {
    $pids = ($processes | ForEach-Object { $_.Id }) -join ", "
    Write-Error "Обнаружено несколько процессов '$ProcessName' (PID: $pids). Замер отклонён: требуется ровно один целевой процесс."
    return
}

$proc = $processes[0]
$procId = $proc.Id
$logicalCores = [Environment]::ProcessorCount

# --- 3. Режим «Холодный старт» (-ColdStart) ---
# Замеряет интервал от запуска процесса до появления оверлея по меткам системного журнала.
# Приложение НЕ перезапускается: читаются метаданные уже работающего процесса и существующий лог.
if ($ColdStart) {
    Write-Host "=================================================================" -ForegroundColor Cyan
    Write-Host " ЗАМЕР ХОЛОДНОГО СТАРТА: $ProcessName (PID: $procId)" -ForegroundColor Cyan
    Write-Host "=================================================================" -ForegroundColor Cyan

    $procStartTimeUtc = $proc.StartTime.ToUniversalTime()
    Write-Host ("Время запуска процесса (OS StartTime): {0:O}" -f $procStartTimeUtc)

    $logDate = $procStartTimeUtc.ToString("yyyy-MM-dd")
    $logDir = Join-Path $env:LOCALAPPDATA "resticker\logs"
    $logFile = Join-Path $logDir "resticker-$logDate.log"

    if (-not (Test-Path $logFile)) {
        Write-Warning "Файл журнала не найден: $logFile. Точный замер по журналу невозможен."
    } else {
        $logInitTime = $null
        $firstOverlayTime = $null
        $audioReadyTime = $null
        $hotkeyReadyTime = $null

        # Построчно ищем события первой сессии текущего процесса
        $logLines = Get-Content $logFile -Encoding UTF8
        foreach ($line in $logLines) {
            if ($line -match '^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z)\s+(INFO|WARN|ERROR)\s+(.*)$') {
                $t = [DateTime]::Parse($matches[1]).ToUniversalTime()
                if ($t -lt $procStartTimeUtc.AddSeconds(-2)) {
                    continue # Пропускаем записи предыдущих запусков до старта текущего процесса
                }
                $msg = $matches[3]

                if ($null -eq $logInitTime -and $msg -match "логирование инициализировано") {
                    $logInitTime = $t
                }
                if ($null -eq $firstOverlayTime -and $msg -match "монитор id=") {
                    $firstOverlayTime = $t
                }
                if ($null -eq $audioReadyTime -and $msg -match "аудиоустройство открыто") {
                    $audioReadyTime = $t
                }
                if ($null -eq $hotkeyReadyTime -and $msg -match "хоткеи combos=") {
                    $hotkeyReadyTime = $t
                }
            }
        }

        if ($null -ne $logInitTime) {
            $deltaLogMs = ($logInitTime - $procStartTimeUtc).TotalMilliseconds
            Write-Host ("Инициализация логирования:           +{0,7:F1} мс ({1:O})" -f $deltaLogMs, $logInitTime)
        }
        if ($null -ne $firstOverlayTime) {
            $deltaOverlayMs = ($firstOverlayTime - $procStartTimeUtc).TotalMilliseconds
            Write-Host ("Создание первого оверлея:            +{0,7:F1} мс ({1:O})" -f $deltaOverlayMs, $firstOverlayTime) -ForegroundColor Green
        } else {
            Write-Host "Метка создания оверлея в текущей сессии журнала не обнаружена." -ForegroundColor Yellow
        }
        if ($null -ne $audioReadyTime) {
            $deltaAudioMs = ($audioReadyTime - $procStartTimeUtc).TotalMilliseconds
            Write-Host ("Готовность аудиомикшера:             +{0,7:F1} мс ({1:O})" -f $deltaAudioMs, $audioReadyTime)
        }
        if ($null -ne $hotkeyReadyTime) {
            $deltaHotkeyMs = ($hotkeyReadyTime - $procStartTimeUtc).TotalMilliseconds
            Write-Host ("Установка глобальных хоткеев:        +{0,7:F1} мс ({1:O})" -f $deltaHotkeyMs, $hotkeyReadyTime)
        }

        if ($null -ne $firstOverlayTime) {
            $totalColdStartMs = ($firstOverlayTime - $procStartTimeUtc).TotalMilliseconds
            Write-Host "-----------------------------------------------------------------"
            Write-Host ("ИТОГО холодный старт до оверлея: {0:F1} мс ({1:F2} с)" -f $totalColdStartMs, ($totalColdStartMs / 1000.0)) -ForegroundColor Cyan
        }
    }
    Write-Host ""
}

# --- 4. Подготовка CSV для потоковой записи ---
if ([string]::IsNullOrWhiteSpace($CsvPath)) {
    $timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
    $CsvPath = Join-Path $env:TEMP "resticker_perf_$timestamp.csv"
}

$csvDir = Split-Path -Path $CsvPath -Parent
if (-not [string]::IsNullOrWhiteSpace($csvDir) -and -not (Test-Path $csvDir)) {
    New-Item -ItemType Directory -Path $csvDir -Force | Out-Null
}

$writer = New-Object System.IO.StreamWriter($CsvPath, $false, [System.Text.Encoding]::UTF8)
$writer.AutoFlush = $true
# Заголовки CSV: сохраняем точные данные с единицами измерения
$writer.WriteLine("Timestamp,ElapsedSec,CpuPercent,WorkingSetMB,PrivateWorkingSetMB,CommitMB,Threads,Handles,GdiObjects,UserObjects")

Write-Host "=================================================================" -ForegroundColor Cyan
Write-Host " ЗАПУСК МОНИТОРИНГА: $ProcessName (PID: $procId)" -ForegroundColor Cyan
Write-Host "=================================================================" -ForegroundColor Cyan
Write-Host "Интервал семплирования: $IntervalSec с"
Write-Host "Плановая длительность:  $DurationSec с ($([Math]::Round($DurationSec / 60, 2)) мин)"
Write-Host "Логических ядер ЦП:     $logicalCores"
Write-Host "Путь к CSV-логу:        $CsvPath"
Write-Host "-----------------------------------------------------------------"

# --- 5. Структуры для вычисления Min/Mean/Max с O(1) памяти ---
# Для поддержки 24-часовых прогонов данные не хранятся в гигантском массиве в RAM,
# а агрегируются на лету.
function New-StatAccumulator {
    return [PSCustomObject]@{
        Count = 0
        Sum   = 0.0
        Min   = [double]::MaxValue
        Max   = [double]::MinValue
    }
}

function Update-StatAccumulator([PSCustomObject]$stat, [double]$val) {
    $stat.Count++
    $stat.Sum += $val
    if ($val -lt $stat.Min) { $stat.Min = $val }
    if ($val -gt $stat.Max) { $stat.Max = $val }
}

$stats = @{
    CpuPercent          = New-StatAccumulator
    WorkingSetMB        = New-StatAccumulator
    PrivateWorkingSetMB = New-StatAccumulator
    CommitMB            = New-StatAccumulator
    Threads             = New-StatAccumulator
    Handles             = New-StatAccumulator
    GdiObjects          = New-StatAccumulator
    UserObjects         = New-StatAccumulator
}

# --- 6. Цикл семплирования ---
$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$proc.Refresh()
$prevTime = [DateTime]::UtcNow
$prevCpuTime = $proc.TotalProcessorTime.TotalSeconds

$sampleIndex = 0

try {
    while ($stopwatch.Elapsed.TotalSeconds -lt $DurationSec) {
        $loopStart = [DateTime]::UtcNow

        # Ожидание следующего такта с компенсацией времени обработки (защита от дрейфа таймера)
        $targetElapsedMs = ($sampleIndex + 1) * ($IntervalSec * 1000.0)
        $delayMs = $targetElapsedMs - $stopwatch.ElapsedMilliseconds
        if ($delayMs -gt 10) {
            Start-Sleep -Milliseconds ([int]$delayMs)
        }

        if ($proc.HasExited) {
            Write-Warning "Процесс resticker.exe завершился во время замера!"
            break
        }

        $now = [DateTime]::UtcNow
        $proc.Refresh()

        $currCpuTime = $proc.TotalProcessorTime.TotalSeconds
        $dt = ($now - $prevTime).TotalSeconds
        $dCpu = $currCpuTime - $prevCpuTime

        # Расчет CPU %: дельта процессорного времени делится на астрономическое время и число ядер
        $cpuPct = 0.0
        if ($dt -gt 0 -and $logicalCores -gt 0) {
            $cpuPct = [Math]::Max(0.0, ($dCpu / ($dt * $logicalCores)) * 100.0)
        }

        $prevTime = $now
        $prevCpuTime = $currCpuTime

        # Замер памяти:
        # WorkingSet: физическая оперативная память, занятая процессом
        $wsMb = $proc.WorkingSet64 / 1MB
        # Commit (PrivateMemorySize): приватная выделенная память процесса
        $commitMb = $proc.PrivateMemorySize64 / 1MB

        # Private Working Set через WMI (если доступен, иначе fallback на WorkingSetPrivate или 0)
        $privateWsMb = 0.0
        try {
            $perfProc = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess = $procId" -ErrorAction SilentlyContinue
            if ($null -ne $perfProc -and $null -ne $perfProc.WorkingSetPrivate) {
                $privateWsMb = $perfProc.WorkingSetPrivate / 1MB
            }
        } catch {
            $privateWsMb = 0.0
        }

        $threadsCount = $proc.Threads.Count
        $handleCount = $proc.HandleCount

        # Замер GDI / User ресурсов (0 = GR_GDIOBJECTS, 1 = GR_USEROBJECTS)
        $gdiCount = [Win32GuiMetrics]::GetGuiResources($proc.Handle, 0)
        $userCount = [Win32GuiMetrics]::GetGuiResources($proc.Handle, 1)

        $elapsedTotalSec = $stopwatch.Elapsed.TotalSeconds

        # Потоковая запись в CSV
        $csvLine = "{0:O},{1:F2},{2:F4},{3:F2},{4:F2},{5:F2},{6},{7},{8},{9}" -f `
            $now, $elapsedTotalSec, $cpuPct, $wsMb, $privateWsMb, $commitMb, $threadsCount, $handleCount, $gdiCount, $userCount
        $writer.WriteLine($csvLine)

        # Обновление статистики
        Update-StatAccumulator $stats.CpuPercent $cpuPct
        Update-StatAccumulator $stats.WorkingSetMB $wsMb
        Update-StatAccumulator $stats.PrivateWorkingSetMB $privateWsMb
        Update-StatAccumulator $stats.CommitMB $commitMb
        Update-StatAccumulator $stats.Threads $threadsCount
        Update-StatAccumulator $stats.Handles $handleCount
        Update-StatAccumulator $stats.GdiObjects $gdiCount
        Update-StatAccumulator $stats.UserObjects $userCount

        $sampleIndex++

        # Печать текущего прогресса в консоль
        $progressPct = [Math]::Min(100.0, ($elapsedTotalSec / $DurationSec) * 100.0)
        Write-Host ("[{0,5:F1}s / {1:F0}s ({2,4:F1}%)] CPU: {3,6:F3}% | WS: {4,6:F1} MB | PrivWS: {5,5:F1} MB | Commit: {6,6:F1} MB | Thr: {7,2} | Hnd: {8,4} | GDI: {9,2}" -f `
            $elapsedTotalSec, $DurationSec, $progressPct, $cpuPct, $wsMb, $privateWsMb, $commitMb, $threadsCount, $handleCount, $gdiCount)
    }
} finally {
    $writer.Flush()
    $writer.Close()
}

# --- 7. Итоговый отчет и верификация по SPEC §13 ---
Write-Host ""
Write-Host "=================================================================" -ForegroundColor Cyan
Write-Host " СВОДНЫЙ ОТЧЁТ ЗАМЕРА ПРОИЗВОДИТЕЛЬНОСТИ ($sampleIndex сэмплов)" -ForegroundColor Cyan
Write-Host "=================================================================" -ForegroundColor Cyan

function Format-SummaryRow($name, $stat, $unit) {
    if ($stat.Count -eq 0) { return }
    $mean = $stat.Sum / $stat.Count
    Write-Host ("{0,-26} Min: {1,8:F3} | Mean: {2,8:F3} | Max: {3,8:F3} {4}" -f $name, $stat.Min, $mean, $stat.Max, $unit)
}

Format-SummaryRow "CPU Utilization"        $stats.CpuPercent          "%"
Format-SummaryRow "Working Set (RAM)"      $stats.WorkingSetMB        "MB"
Format-SummaryRow "Private Working Set"    $stats.PrivateWorkingSetMB "MB"
Format-SummaryRow "Commit (Private Bytes)" $stats.CommitMB            "MB"
Format-SummaryRow "Threads Count"          $stats.Threads             ""
Format-SummaryRow "Handles Count"          $stats.Handles             ""
Format-SummaryRow "GDI Objects"            $stats.GdiObjects          ""
Format-SummaryRow "User Objects"           $stats.UserObjects         ""

Write-Host "-----------------------------------------------------------------"

# --- 8. Вердикт против таблицы SPEC.md §13 ---
# SPEC §13:
# Сценарий "Только статика" (25 стикеров, 3 монитора): CPU < 0.3%, RAM < 200 MB, GPU ~0%
# Сценарий "1 видео / 5 видео": CPU < 6%..8%, RAM < 600 MB
# Требование покоя: "в полном покое программа не должна просыпаться вообще"
Write-Host "ВЕРДИКТ ПРОТИВ ТРЕБОВАНИЙ SPEC.md §13:" -ForegroundColor Cyan

$meanCpu = if ($stats.CpuPercent.Count -gt 0) { $stats.CpuPercent.Sum / $stats.CpuPercent.Count } else { 0.0 }
$maxCpu = $stats.CpuPercent.Max
$meanWs = if ($stats.WorkingSetMB.Count -gt 0) { $stats.WorkingSetMB.Sum / $stats.WorkingSetMB.Count } else { 0.0 }
$meanCommit = if ($stats.CommitMB.Count -gt 0) { $stats.CommitMB.Sum / $stats.CommitMB.Count } else { 0.0 }
$meanThreads = if ($stats.Threads.Count -gt 0) { $stats.Threads.Sum / $stats.Threads.Count } else { 0.0 }

# 1. Анализ CPU
if ($meanCpu -lt 0.3) {
    Write-Host (" [PASS] CPU в покое: среднее {0:F3}% (норматив статики < 0.3%, видео < 6.0%)" -f $meanCpu) -ForegroundColor Green
} else {
    Write-Host (" [FAIL] CPU выше нормы: среднее {0:F3}% (норматив статики < 0.3%)" -f $meanCpu) -ForegroundColor Red
}

# 2. Анализ требования «ноль пробуждений в покое»
# Если CPU строго 0.000%, программа не просыпается на тики таймеров.
# При наличии ненулевого фонового расхода (например, 0.02%), таймеры координатора или WebView2 активны.
if ($maxCpu -lt 0.001) {
    Write-Host " [PASS] Полный покой: нулевые пробуждения подтверждены (CPU = 0.000%)." -ForegroundColor Green
} else {
    Write-Host (" [WARN] Фоновые пробуждения: CPU в покое ненулевой (среднее {0:F3}%, макс {1:F3}%)." -f $meanCpu, $maxCpu) -ForegroundColor Yellow
    Write-Host "        Причина: 1-секундный тик координатора (OverlayMessage::Tick), фоновые потоки WebView2 и опрос трекера." -ForegroundColor Gray
}

# 3. Анализ памяти против потолка 200 МБ для 25 стикеров
Write-Host (" Память сейчас: Working Set = {0:F1} МБ, Commit Charge = {1:F1} МБ." -f $meanWs, $meanCommit)
if ($meanWs -lt 200.0 -and $meanCommit -lt 200.0) {
    Write-Host " [PASS] Текущий расход укладывается в потолок 200 МБ для текущего числа стикеров." -ForegroundColor Green
} else {
    Write-Host " [FAIL/RISK] Commit Charge ({0:F1} МБ) близок к пределу или превышает лимит 200 МБ." -f $meanCommit -ForegroundColor Yellow
}

Write-Host "=================================================================" -ForegroundColor Cyan
Write-Host "Отчёт сохранён в: $CsvPath" -ForegroundColor Cyan
Write-Host "=================================================================" -ForegroundColor Cyan
