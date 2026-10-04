# Prints the hardware details for a steadycheck test report as Markdown tables, ready to paste.
# Leaves out serial numbers, user and computer names, network details and product keys.
# Run:  powershell -ExecutionPolicy Bypass -File .\collect-info.ps1
# If steadycheck*.exe is in the same folder as this script, its version is read from its JSON output.
$ErrorActionPreference = 'Stop'
# On any other failure print only its kind: PowerShell's error text would show this script's full path (with the user name)
trap { Write-Host "collect-info.ps1 stopped: could not read the hardware details ($($_.CategoryInfo.Category))"; exit 1 }

# Trimmed text that cannot break a Markdown table cell
function Cell($v) { if ($null -eq $v) { return '' }; return ([string]$v).Trim() -replace '\|', '/' }

# Only the listed properties (and each class's key properties, none of them a serial number) are requested,
# so serial numbers never enter this script
$os = Get-CimInstance Win32_OperatingSystem -Property Caption, Version | Select-Object Caption, Version
$cpus = @(Get-CimInstance Win32_Processor -Property Name, NumberOfCores, NumberOfLogicalProcessors | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors)
$board = Get-CimInstance Win32_BaseBoard -Property Manufacturer, Product | Select-Object Manufacturer, Product
$bios = Get-CimInstance Win32_BIOS -Property Manufacturer, SMBIOSBIOSVersion, ReleaseDate | Select-Object Manufacturer, SMBIOSBIOSVersion, ReleaseDate
$dimms = @(Get-CimInstance Win32_PhysicalMemory -Property Manufacturer, PartNumber, Capacity, Speed, ConfiguredClockSpeed | Select-Object Manufacturer, PartNumber, Capacity, Speed, ConfiguredClockSpeed)

$cpuName = (@($cpus | ForEach-Object { Cell $_.Name } | Sort-Object -Unique)) -join ' + '
$cores = ($cpus | Measure-Object NumberOfCores -Sum).Sum
$threads = ($cpus | Measure-Object NumberOfLogicalProcessors -Sum).Sum
$biosDate = ''
if ($bios.ReleaseDate) { $biosDate = $bios.ReleaseDate.ToString('yyyy-MM-dd') }
$totalGiB = [math]::Round([double](($dimms | Measure-Object Capacity -Sum).Sum) / 1GB, 1)

$version = 'not found (put steadycheck*.exe in the same folder as this script)'
$exe = Get-ChildItem -Path $PSScriptRoot -Filter 'steadycheck*.exe' -File | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($exe) {
    # A one-second, one-thread CPU run, only to read "version" from the JSON on stdout.
    # Its stderr is dropped; 'Continue' keeps Windows PowerShell 5.1 from treating stderr lines as errors.
    # If it cannot start (blocked, damaged file), only a neutral note is printed, not the error text with the full path.
    $ErrorActionPreference = 'Continue'
    $version = "(could not run $($exe.Name))"
    try { $json = & $exe.FullName cpu --seconds 1 --iters 4096 --threads 1 2>$null | Out-String; $version = "unknown ($($exe.Name) printed no version)" } catch { }
    $ErrorActionPreference = 'Stop'
    try { $v = ($json | ConvertFrom-Json).version; if ($v) { $version = "$v ($($exe.Name))" } } catch { }
}

$lines = @(
    '### Hardware (tools/collect-info.ps1)'
    ''
    '| Item | Value |'
    '|---|---|'
    "| Windows | $(Cell $os.Caption) $(Cell $os.Version) |"
    "| CPU | $cpuName |"
    "| Cores / threads | $cores / $threads |"
    "| Motherboard | $(Cell $board.Manufacturer) / $(Cell $board.Product) |"
    "| BIOS | $(Cell $bios.Manufacturer) / $(Cell $bios.SMBIOSBIOSVersion) / $biosDate |"
    "| Memory total | $totalGiB GiB in $($dimms.Count) module(s) |"
    "| steadycheck | $version |"
    ''
    '| Module | Manufacturer | Part number | Capacity | Rated speed | Configured speed |'
    '|---|---|---|---|---|---|'
)
$n = 0
foreach ($d in $dimms) {
    $n++
    $gib = [math]::Round([double]$d.Capacity / 1GB, 1)
    $lines += "| $n | $(Cell $d.Manufacturer) | $(Cell $d.PartNumber) | $gib GiB | $(Cell $d.Speed) | $(Cell $d.ConfiguredClockSpeed) |"
}
$lines
