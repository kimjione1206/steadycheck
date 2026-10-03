# SDE -mix 출력의 전역 명령 실행 횟수를 공개 불량 명령 범주로 묶어 비율을 낸다
param(
    [Parameter(Mandatory)][string]$Mix,
    [Parameter(Mandatory)][string]$Name
)
$ErrorActionPreference = 'Stop'

# 위에서부터 처음 맞는 범주 하나(대소문자 무시), 안 맞으면 other.
# XED 명령 이름은 lock 접두를 ADD_LOCK 처럼 뒤에 붙인다
$cats = [ordered]@{
    fma_f64_packed  = '^VF(N)?M(ADD|SUB|ADDSUB|SUBADD)\d{3}PD'
    fma_f32_packed  = '^VF(N)?M(ADD|SUB|ADDSUB|SUBADD)\d{3}PS'
    fma_scalar      = '^VF(N)?M(ADD|SUB)\d{3}S[DS]'
    vec_int_mul     = '^VPMUL'
    gather          = 'GATHER'
    permute_shuffle = '^VPERM|^VSHUF|^VPSHUF|^VPALIGNR|^VPUNPCK'
    vec_int_other   = '^VP(ADD|SUB|XOR|AND|OR|SLL|SRL|SRA|ROL|ROR|TERNLOG|CMP|MAX|MIN|BLEND|BROADCAST)'
    vec_move        = '^VMOV|^MOVDQ|^MOVAP|^MOVUP'
    x87             = '^(?!FXSAVE|FXRSTOR)F[A-Z0-9]+$'
    aes             = '^V?AES'
    rep_string      = '^REP_|^REPE_|^REPNE_'
    bmi             = '^(ANDN|BEXTR|BLSI|BLSMSK|BLSR|BZHI|MULX|PDEP|PEXT|RORX|SARX|SHLX|SHRX|TZCNT|LZCNT|POPCNT)$'
    atomic_lock     = '_LOCK$|CMPXCHG|^XCHG|^XADD'
    scalar_int_mul  = '^IMUL|^MUL$'
}

# "# $global-dynamic-counts" ~ "# END_GLOBAL_DYNAMIC_STATS": "명령이름  횟수" 줄, "*" 줄은 묶음 통계(*total 만 대조용)
$counts = @{}
$total = $null
$in = $false
foreach ($l in Get-Content $Mix) {
    if ($l -match '^#\s*\$global-dynamic-counts') { $in = $true; continue }
    if (-not $in) { continue }
    if ($l -match '^#\s*END_GLOBAL_DYNAMIC_STATS') { break }
    if ($l -match '^\*total\s+(\d+)\s*$') { $total = [long]$Matches[1] }
    elseif ($l -match '^([A-Z][A-Z0-9_]*)\s+(\d+)\s*$') { $counts[$Matches[1]] = [long]$Matches[2] }
}
$sum = [long]0
foreach ($v in $counts.Values) { $sum += $v }
if (-not $counts.Count -or $sum -ne $total) { throw "$Mix 전역 집계를 못 읽음: 명령 $($counts.Count)개, 합 $sum, *total $total" }

$byCat = [ordered]@{}
foreach ($c in @($cats.Keys) + 'other') { $byCat[$c] = [Collections.Generic.List[object]]::new() }
foreach ($op in $counts.Keys) {
    $cat = 'other'
    foreach ($c in $cats.Keys) { if ($op -match $cats[$c]) { $cat = $c; break } }
    $byCat[$cat].Add([pscustomobject]@{ op = $op; count = $counts[$op] })
}
foreach ($c in $byCat.Keys) {
    $ops = @($byCat[$c] | Sort-Object count -Descending)
    $n = [long]0
    foreach ($o in $ops) { $n += $o.count }
    [pscustomobject]@{
        name     = $Name
        category = $c
        count    = $n
        # 소수 6자리 — 아주 드문 명령은 0 으로 반올림되니 썼는지는 count 로 본다
        percent  = (100.0 * $n / $sum).ToString('0.######', [cultureinfo]::InvariantCulture)
        top      = ($ops | Select-Object -First 3 | ForEach-Object { "$($_.op) $($_.count)" }) -join '; '
    }
}
