# 리눅스에서 steadycheck cpu 를 DynamoRIO 아래 drfault 와 함께 돌리고, 주입 기록과 검사기가 잡은 오류를 맞춰 본다
param(
    [Parameter(Mandatory)][string]$Drrun,
    [Parameter(Mandatory)][string]$Client,
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][ValidateSet('fma', 'wide')][string]$Kernel,
    [Parameter(Mandatory)][string]$Ops,
    [long]$Every = 0,
    [int]$Bit = 20,
    [ValidateSet('worker', 'main')][string]$Thread = 'worker'
)
$ErrorActionPreference = 'Stop'

$log = Join-Path ([IO.Path]::GetTempPath()) "drfault-$([guid]::NewGuid()).jsonl"
try {
    $out = & $Drrun -c $Client -ops $Ops -every $Every -bit $Bit -thread $Thread -log $log `
        -- $Exe cpu --isa avx2 --kernel $Kernel --threads 4 --iters 4096 --seconds 20 | Out-String
    $lines = @(Get-Content $log | ForEach-Object { $_ | ConvertFrom-Json })
} finally {
    Remove-Item $log -ErrorAction SilentlyContinue
}
$r = try { $out | ConvertFrom-Json } catch { $null }
if (-not $r.verdict) { throw "steadycheck 판정 없음 ($Kernel every $Every): $out" }

$inj = @($lines | Where-Object { $null -ne $_.tid })
$exitMs = @($lines | Where-Object { $null -ne $_.exit_ms })[0].exit_ms
$e = $r.cpu.error
# 리눅스에선 일꾼이 CPU 에 고정되지 않아 하드웨어 CPU 대조는 못 한다.
# 참고용: 시작 순서(주 스레드 0, 일꾼 k = k+1)로 짐작한 일꾼 번호와 검사기가 보고한 일꾼 번호(error.cpu)
$injWorker = if ($inj.Count) { [int]$inj[0].seq - 1 } else { $null }
$errWorker = if ($e) { [int]$e.cpu } else { $null }
# 첫 주입 → 검출(ms): 검사기 시각(시작 기준)을 클라이언트 종료 시각(벽시계)에 맞춰 옮긴다 — 검출 ≈ exit_ms - (elapsed_ms - at_ms)
$detectMs = if ($e -and $inj.Count -and $exitMs) { [long]$exitMs - ([long]$r.cpu.elapsed_ms - [long]$e.at_ms) - [long]$inj[0].ms } else { $null }
[pscustomobject]@{
    kernel          = $Kernel
    every           = $Every
    verdict         = $r.verdict
    golden_unstable = [bool]$r.cpu.golden_unstable
    injections      = $inj.Count
    inj_threads     = @($inj | ForEach-Object tid | Sort-Object -Unique).Count
    inj_tid         = if ($inj.Count) { $inj[0].tid } else { $null }
    inj_worker      = $injWorker
    err_worker      = $errWorker
    worker_same     = if ($null -ne $injWorker -and $null -ne $errWorker) { $injWorker -eq $errWorker } else { $null }
    err_kernel      = $e.kernel
    detect_ms       = $detectMs
}
