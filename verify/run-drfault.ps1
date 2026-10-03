# 리눅스에서 steadycheck cpu 를 DynamoRIO 아래 drfault 와 함께 돌리고, 주입 기록과 검사기가 잡은 오류를 맞춰 본다
param(
    [Parameter(Mandatory)][string]$Drrun,
    [Parameter(Mandatory)][string]$Client,
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][ValidateSet('fma', 'wide')][string]$Kernel,
    [Parameter(Mandatory)][string]$Ops,
    [long]$Every = 0,
    # 0 이상이면 -every 대신 "결과 하위 12비트가 이 값일 때마다"
    [int]$Match = -1,
    # 스레드별 대상 명령 실행 수가 이 값을 넘은 뒤부터만 주입
    [long]$After = 0,
    [long]$Iters = 4096,
    [int]$Bit = 20,
    [ValidateSet('worker', 'main', 'main+worker')][string]$Thread = 'worker',
    # 레지스터 왕복만 하고 0 을 XOR (대조군)
    [switch]$Mask0
)
$ErrorActionPreference = 'Stop'

$log = Join-Path ([IO.Path]::GetTempPath()) "drfault-$([guid]::NewGuid()).jsonl"
$opt = @('-ops', $Ops, '-every', $Every, '-after', $After, '-bit', $Bit, '-thread', $Thread, '-log', $log)
if ($Match -ge 0) { $opt += '-match', $Match }
if ($Mask0) { $opt += '-mask0' }
try {
    $out = & $Drrun -c $Client @opt `
        -- $Exe cpu --isa avx2 --kernel $Kernel --threads 4 --iters $Iters --seconds 20 | Out-String
    $lines = @(Get-Content $log | ForEach-Object { $_ | ConvertFrom-Json })
} finally {
    Remove-Item $log -ErrorAction SilentlyContinue
}
$r = try { $out | ConvertFrom-Json } catch { $null }
if (-not $r.verdict) { throw "steadycheck 판정 없음 ($Kernel every $Every match $Match): $out" }

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
    inj_main        = @($inj | Where-Object { $_.seq -eq 0 }).Count
    inj_nonmain     = @($inj | Where-Object { $_.seq -ne 0 }).Count
    inj_threads     = @($inj | ForEach-Object tid | Sort-Object -Unique).Count
    inj_tid         = if ($inj.Count) { $inj[0].tid } else { $null }
    inj_worker      = $injWorker
    err_worker      = $errWorker
    worker_same     = if ($null -ne $injWorker -and $null -ne $errWorker) { $injWorker -eq $errWorker } else { $null }
    err_kernel      = $e.kernel
    err_block       = $e.block
    detect_ms       = $detectMs
}
