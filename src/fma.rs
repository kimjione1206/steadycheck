//! FMA 커널: 곱셈-덧셈 장치(FMA)를 계속 돌린다.
//! 입력은 부호·지수까지 무작위(2^-16 ~ 2^15) — 대규모 조사에서 FMA 불량이 입력 지수 비트에 치우쳤다.
//! 매 반복의 누적값 비트를 정수 요약(h)에 회전-XOR 로 새겨, 한 번 틀린 결과가 반올림으로 사라지지 않게 한다.

use crate::kernel::{splitmix64, Flip, Isa};

pub const LANES: usize = 32;
const C: f64 = 1.5;

/// 난수 → 실수 비트: 부호 = 비트 5, 지수 = 0x3EF + 하위 5비트 (2^-16 ~ 2^15), 가수 = 상위 52비트
#[inline(always)]
fn input_bits(v: u64) -> u64 {
    ((v << 58) & 0x8000_0000_0000_0000) | (((v & 0x1F) + 0x3EF) << 52) | (v >> 12)
}

struct State {
    r: [u64; LANES],
    a: [f64; LANES],
    h: [u64; LANES],
}

// 시작 누적값도 h 에 새겨 둔다: 뒤집힌 낮은 비트가 첫 반올림에 사라져도 흔적이 남는다
fn start_trace(a: &[f64; LANES]) -> [u64; LANES] {
    let mut h = [0u64; LANES];
    for i in 0..LANES {
        h[i] = a[i].to_bits();
    }
    h
}

fn seed_state(seed: u64, flip: Option<Flip>) -> State {
    let mut r = [0u64; LANES];
    let mut a = [0f64; LANES];
    for i in 0..LANES {
        r[i] = splitmix64(seed.wrapping_mul(LANES as u64).wrapping_add(i as u64)) | 1;
        a[i] = f64::from_bits(input_bits(r[i].rotate_left(32)));
    }
    if let Some(f) = flip {
        let lane = f.lane % (2 * LANES);
        let bit = 1u64 << (f.bit % 64);
        if lane < LANES {
            r[lane] ^= bit;
        } else {
            a[lane - LANES] = f64::from_bits(a[lane - LANES].to_bits() ^ bit);
        }
    }
    let h = start_trace(&a);
    State { r, a, h }
}

fn fold(s: &State) -> u64 {
    let mut x = 0u64;
    for i in 0..LANES {
        x = x.rotate_left(7) ^ s.r[i] ^ s.a[i].to_bits().rotate_left(17) ^ s.h[i].rotate_left(29);
    }
    x
}

/// fault: (줄, 반복 번호, 입력 비트) → 결과 비트에 XOR 할 값. 평소엔 0 — 검출 채점용 불량 흉내 자리
fn step_scalar_with(s: &mut State, iters: u64, mut fault: impl FnMut(usize, u64, u64) -> u64) {
    for n in 0..iters {
        for i in 0..LANES {
            let mut v = s.r[i];
            v ^= v << 13;
            v ^= v >> 7;
            v ^= v << 17;
            s.r[i] = v;
            let ub = input_bits(v);
            // mul_add 는 한 번만 반올림 — 하드웨어 FMA 와 비트 단위로 같다
            let fused = f64::from_bits(ub).mul_add(C, s.a[i]);
            s.a[i] = f64::from_bits(fused.to_bits() ^ fault(i, n, ub));
            s.h[i] = s.h[i].rotate_left(1) ^ s.a[i].to_bits();
        }
    }
}

fn step_scalar(s: &mut State, iters: u64) {
    step_scalar_with(s, iters, |_, _, _| 0)
}

fn run_scalar(mut s: State, iters: u64) -> u64 {
    step_scalar(&mut s, iters);
    fold(&s)
}

/// 검출 채점용: 불량 흉내를 넣은 스칼라 계산 (crate::fault 만 쓴다)
pub(crate) fn run_scalar_faulty(seed: u64, iters: u64, fault: impl FnMut(usize, u64, u64) -> u64) -> u64 {
    let mut s = seed_state(seed, None);
    step_scalar_with(&mut s, iters, fault);
    fold(&s)
}

// AVX2 는 계산용 임시 칸이 16개뿐이라 32줄을 16줄씩 두 번 계산한다 (줄끼리 독립이라 결과 동일)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn run_avx2(mut s: State, iters: u64) -> u64 {
    use std::arch::x86_64::*;
    const V: usize = 4;
    let c = _mm256_set1_pd(C);
    let low5 = _mm256_set1_epi64x(0x1F);
    let exp0 = _mm256_set1_epi64x(0x3EF);
    let sign = _mm256_set1_epi64x(i64::MIN);
    for half in 0..2 {
        let o = half * 16;
        let mut r = [_mm256_setzero_si256(); V];
        let mut a = [_mm256_setzero_pd(); V];
        let mut h = [_mm256_setzero_si256(); V];
        for k in 0..V {
            r[k] = _mm256_loadu_si256(s.r.as_ptr().add(o + 4 * k) as *const __m256i);
            a[k] = _mm256_loadu_pd(s.a.as_ptr().add(o + 4 * k));
            h[k] = _mm256_loadu_si256(s.h.as_ptr().add(o + 4 * k) as *const __m256i);
        }
        for _ in 0..iters {
            for k in 0..V {
                let mut v = r[k];
                v = _mm256_xor_si256(v, _mm256_slli_epi64(v, 13));
                v = _mm256_xor_si256(v, _mm256_srli_epi64(v, 7));
                v = _mm256_xor_si256(v, _mm256_slli_epi64(v, 17));
                r[k] = v;
                let sg = _mm256_and_si256(_mm256_slli_epi64(v, 58), sign);
                let ex = _mm256_slli_epi64(_mm256_add_epi64(_mm256_and_si256(v, low5), exp0), 52);
                let u = _mm256_castsi256_pd(_mm256_or_si256(_mm256_or_si256(sg, ex), _mm256_srli_epi64(v, 12)));
                a[k] = _mm256_fmadd_pd(u, c, a[k]);
                let rot = _mm256_or_si256(_mm256_slli_epi64(h[k], 1), _mm256_srli_epi64(h[k], 63));
                h[k] = _mm256_xor_si256(rot, _mm256_castpd_si256(a[k]));
            }
        }
        for k in 0..V {
            _mm256_storeu_si256(s.r.as_mut_ptr().add(o + 4 * k) as *mut __m256i, r[k]);
            _mm256_storeu_pd(s.a.as_mut_ptr().add(o + 4 * k), a[k]);
            _mm256_storeu_si256(s.h.as_mut_ptr().add(o + 4 * k) as *mut __m256i, h[k]);
        }
    }
    fold(&s)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn run_avx512(mut s: State, iters: u64) -> u64 {
    use std::arch::x86_64::*;
    const V: usize = LANES / 8;
    let c = _mm512_set1_pd(C);
    let low5 = _mm512_set1_epi64(0x1F);
    let exp0 = _mm512_set1_epi64(0x3EF);
    let sign = _mm512_set1_epi64(i64::MIN);
    let mut r = [_mm512_setzero_si512(); V];
    let mut a = [_mm512_setzero_pd(); V];
    let mut h = [_mm512_setzero_si512(); V];
    for k in 0..V {
        r[k] = _mm512_loadu_si512(s.r.as_ptr().add(8 * k) as *const __m512i);
        a[k] = _mm512_loadu_pd(s.a.as_ptr().add(8 * k));
        h[k] = _mm512_loadu_si512(s.h.as_ptr().add(8 * k) as *const __m512i);
    }
    for _ in 0..iters {
        for k in 0..V {
            let mut v = r[k];
            v = _mm512_xor_si512(v, _mm512_slli_epi64::<13>(v));
            v = _mm512_xor_si512(v, _mm512_srli_epi64::<7>(v));
            v = _mm512_xor_si512(v, _mm512_slli_epi64::<17>(v));
            r[k] = v;
            let sg = _mm512_and_si512(_mm512_slli_epi64::<58>(v), sign);
            let ex = _mm512_slli_epi64::<52>(_mm512_add_epi64(_mm512_and_si512(v, low5), exp0));
            let u = _mm512_castsi512_pd(_mm512_or_si512(_mm512_or_si512(sg, ex), _mm512_srli_epi64::<12>(v)));
            a[k] = _mm512_fmadd_pd(u, c, a[k]);
            h[k] = _mm512_xor_si512(_mm512_rol_epi64::<1>(h[k]), _mm512_castpd_si512(a[k]));
        }
    }
    for k in 0..V {
        _mm512_storeu_si512(s.r.as_mut_ptr().add(8 * k) as *mut __m512i, r[k]);
        _mm512_storeu_pd(s.a.as_mut_ptr().add(8 * k), a[k]);
        _mm512_storeu_si512(s.h.as_mut_ptr().add(8 * k) as *mut __m512i, h[k]);
    }
    fold(&s)
}

/// 호출자(kernel::run_block)가 isa 지원 여부를 먼저 확인한다.
pub fn run(isa: Isa, seed: u64, iters: u64, flip: Option<Flip>) -> u64 {
    let s = seed_state(seed, flip);
    match isa {
        Isa::Scalar => run_scalar(s, iters),
        #[cfg(target_arch = "x86_64")]
        Isa::Avx2 => unsafe { run_avx2(s, iters) },
        #[cfg(target_arch = "x86_64")]
        Isa::Avx512 => unsafe { run_avx512(s, iters) },
        #[allow(unreachable_patterns)]
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 정답값: 2026-09-30 맥 ARM·Rosetta x86 스칼라·AVX2 에서 같은 값으로 실측
    #[test]
    fn known_answers() {
        assert_eq!(run(Isa::Scalar, 0, 1000, None), 0x4DBE_FCB2_9D16_FD80);
        assert_eq!(run(Isa::Scalar, 1, 1000, None), 0x4417_E8DE_48A3_EF32);
        assert_eq!(run(Isa::Scalar, 42, 1000, None), 0xEC37_5B30_4C88_1529);
    }

    #[test]
    fn simd_matches_scalar() {
        for isa in [Isa::Avx2, Isa::Avx512] {
            if !isa.supported() {
                eprintln!("fma {isa:?} 미지원 — 건너뜀");
                continue;
            }
            for seed in 0..32 {
                for iters in [1, 1000, 4097] {
                    assert_eq!(run(isa, seed, iters, None), run(Isa::Scalar, seed, iters, None), "{isa:?} seed={seed} iters={iters}");
                }
            }
        }
    }

    // 정수 상태(0~31)와 누적 실수(32~63) 어느 쪽의 어느 비트를 뒤집어도 결과가 달라져야 한다
    #[test]
    fn every_flip_changes_result() {
        let clean = run(Isa::Scalar, 7, 1000, None);
        for lane in 0..2 * LANES {
            for bit in [0, 1, 31, 32, 51, 52, 62, 63] {
                assert_ne!(run(Isa::Scalar, 7, 1000, Some(Flip { lane, bit })), clean, "lane={lane} bit={bit}");
            }
        }
    }

    #[test]
    fn flip_targets_the_right_value() {
        let mut s = seed_state(7, None);
        s.r[3] ^= 1 << 40;
        s.h = start_trace(&s.a);
        assert_eq!(run(Isa::Scalar, 7, 1000, Some(Flip { lane: 3, bit: 40 })), run_scalar(s, 1000));
        let mut s = seed_state(7, None);
        s.a[5] = f64::from_bits(s.a[5].to_bits() ^ 1);
        s.h = start_trace(&s.a);
        assert_eq!(run(Isa::Scalar, 7, 1000, Some(Flip { lane: LANES + 5, bit: 0 })), run_scalar(s, 1000));
    }

    // 입력 지수가 32종 모두 나오고, 부호도 양쪽 다 나와야 한다
    #[test]
    fn inputs_cover_exponents_and_signs() {
        let mut exps = std::collections::BTreeSet::new();
        let mut signs = std::collections::BTreeSet::new();
        for i in 0..2000 {
            let b = input_bits(splitmix64(i));
            exps.insert((b >> 52) & 0x7FF);
            signs.insert(b >> 63);
        }
        assert_eq!(exps.len(), 32);
        assert_eq!((*exps.first().unwrap(), *exps.last().unwrap()), (0x3EF, 0x40E));
        assert_eq!(signs.len(), 2);
    }

    // 누적값은 오래 돌아도 유한하고 비정규수가 아니어야 한다
    #[test]
    fn accumulators_stay_finite_and_normal() {
        let mut s = seed_state(3, None);
        step_scalar(&mut s, 1 << 16);
        assert!(s.a.iter().all(|v| v.is_finite() && !v.is_subnormal()));
    }

    #[test]
    fn fault_hook_zero_is_identity() {
        for seed in [0, 5] {
            assert_eq!(run_scalar_faulty(seed, 1000, |_, _, _| 0), run(Isa::Scalar, seed, 1000, None));
        }
        assert_ne!(run_scalar_faulty(5, 1000, |l, n, _| if l == 3 && n == 10 { 1 } else { 0 }), run(Isa::Scalar, 5, 1000, None));
    }
}
