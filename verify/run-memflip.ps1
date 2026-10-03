# steadycheck mem 을 띄우고 바깥에서 memflip 으로 비트를 건드린 뒤, 잡았는지·어디·어느 비트를 잡았는지 대조한다
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Flip,
    [Parameter(Mandatory)][ValidateSet('once', 'stuck0', 'stuck1', 'watch')][string]$Mode,
    [int]$Bit = 0,
    [long]$Word = 0,
    [int]$DelayMs = 2000
)
$ErrorActionPreference = 'Stop'

# "0x…" 16진 문자열 → uint64 (PowerShell 정수 넘침 방지)
function Hex([string]$s) { [Convert]::ToUInt64($s.Substring(2), 16) }

$out = New-TemporaryFile
try {
    $p = Start-Process $Exe 'mem --mb 512 --seconds 20 --threads 4' -RedirectStandardOutput $out.FullName -PassThru -NoNewWindow
    $line = & $Flip --pid $p.Id --mode $Mode --bit $Bit --word $Word --delay-ms $DelayMs --seconds 25 | Select-Object -Last 1
    $flipCode = $LASTEXITCODE
    $p.WaitForExit()
    if ($flipCode -ne 0) { throw "memflip 종료 코드 $flipCode ($Mode bit $Bit word $Word)" }
    $f = $line | ConvertFrom-Json
    $r = Get-Content $out.FullName -Raw | ConvertFrom-Json
} finally {
    Remove-Item $out.FullName -ErrorAction SilentlyContinue
}

$e = $r.mem.error
$caught = $r.verdict -eq 'FAIL'
$bitOk = $false
$header = $null
if ($e) {
    $bitOk = ((Hex $e.expected) -bxor (Hex $e.actual)) -eq ([uint64]1 -shl $Bit)
    # 영역 시작 ~ 버퍼 시작 거리 = (대상 주소 - 영역 시작) - 검사기가 말한 버퍼 안 위치
    $header = [long]((Hex $f.addr) - (Hex $f.region_base)) - [long]$e.offset_bytes
}
# 같은 칸·같은 비트를 짚었는지
$placeOk = $bitOk -and $header -ge 0 -and $header -lt 4096 -and $header % 8 -eq 0
$ok = switch ($Mode) {
    'watch' { $r.verdict -eq 'PASS' }
    'once' { (-not $caught -and $r.verdict -eq 'PASS') -or ($caught -and $placeOk) }
    default { $caught -and $placeOk }
}
[pscustomobject]@{
    mode    = $Mode
    bit     = $Bit
    word    = $Word
    verdict = $r.verdict
    caught  = $caught
    bit_ok  = $bitOk
    header  = $header
    writes  = $f.writes
    ok      = [bool]$ok
}
