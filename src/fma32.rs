//! 단정밀도 FMA 커널: 32비트 실수 곱셈-덧셈. 대규모 조사에서 단정밀도 벡터 FMA 만 틀린 불량이 보고됐다.
//! 입력은 부호·지수 무작위(2^-8 ~ 2^7). 누적값 비트를 정수 요약(h)에 회전-XOR 로 새긴다.

use crate::kernel::{splitmix64, Flip, Isa};

pub const LANES: usize = 32;
const C: f32 = 1.5;

/// 난수 → 실수 비트: 부호 = 비트 4, 지수 = 0x77 + 하위 4비트 (2^-8 ~ 2^7), 가수 = 상위 23비트
#[inline(always)]
fn input_bits(v: u32) -> u32 {
    ((v << 27) & 0x8000_0000) | (((v & 0xF) + 0x77) << 23) | (v >> 9)
}

struct State {
    r: [u32; LANES],
    a: [f32; LANES],
    h: [u32; LANES],
}

fn seed_state(seed: u64, flip: Option<Flip>) -> State {
    let mut r = [0u32; LANES];
    let mut a = [0f32; LANES];
    for i in 0..LANES {
        let z = splitmix64(seed.wrapping_mul(LANES as u64).wrapping_add(i as u64));
        r[i] = (z as u32) | 1;
        a[i] = f32::from_bits(input_bits((z >> 32) as u32));
    }
    if let Some(f) = flip {
        let lane = f.lane % (2 * LANES);
        let bit = 1u32 << (f.bit % 32);
        if lane < LANES {
            r[lane] ^= bit;
        } else {
            a[lane - LANES] = f32::from_bits(a[lane - LANES].to_bits() ^ bit);
        }
    }
    // 시작 누적값도 h 에 새겨 둔다
    let mut h = [0u32; LANES];
    for i in 0..LANES {
        h[i] = a[i].to_bits();
    }
    State { r, a, h }
}

fn fold(s: &State) -> u64 {
    let mut x = 0u64;
    for i in 0..LANES {
        x = x.rotate_left(7) ^ s.r[i] as u64 ^ (s.a[i].to_bits() as u64).rotate_left(17) ^ (s.h[i] as u64).rotate_left(29);
    }
    x
}

fn step_scalar(s: &mut State, iters: u64) {
    for _ in 0..iters {
        for i in 0..LANES {
            // 32비트 xorshift (13, 17, 5)
            let mut v = s.r[i];
            v ^= v << 13;
            v ^= v >> 17;
            v ^= v << 5;
            s.r[i] = v;
            // mul_add 는 한 번만 반올림 — 하드웨어 FMA 와 비트 단위로 같다
            s.a[i] = f32::from_bits(input_bits(v)).mul_add(C, s.a[i]);
            s.h[i] = s.h[i].rotate_left(1) ^ s.a[i].to_bits();
        }
    }
}

fn run_scalar(mut s: State, iters: u64) -> u64 {
    step_scalar(&mut s, iters);
    fold(&s)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn run_avx2(mut s: State, iters: u64) -> u64 {
    use std::arch::x86_64::*;
    const V: usize = LANES / 8;
    let c = _mm256_set1_ps(C);
    let low4 = _mm256_set1_epi32(0xF);
    let exp0 = _mm256_set1_epi32(0x77);
    let sign = _mm256_set1_epi32(i32::MIN);
    let mut r = [_mm256_setzero_si256(); V];
    let mut a = [_mm256_setzero_ps(); V];
    let mut h = [_mm256_setzero_si256(); V];
    for k in 0..V {
        r[k] = _mm256_loadu_si256(s.r.as_ptr().add(8 * k) as *const __m256i);
        a[k] = _mm256_loadu_ps(s.a.as_ptr().add(8 * k));
        h[k] = _mm256_loadu_si256(s.h.as_ptr().add(8 * k) as *const __m256i);
    }
    for _ in 0..iters {
        for k in 0..V {
            let mut v = r[k];
            v = _mm256_xor_si256(v, _mm256_slli_epi32(v, 13));
            v = _mm256_xor_si256(v, _mm256_srli_epi32(v, 17));
            v = _mm256_xor_si256(v, _mm256_slli_epi32(v, 5));
            r[k] = v;
            let sg = _mm256_and_si256(_mm256_slli_epi32(v, 27), sign);
            let ex = _mm256_slli_epi32(_mm256_add_epi32(_mm256_and_si256(v, low4), exp0), 23);
            let u = _mm256_castsi256_ps(_mm256_or_si256(_mm256_or_si256(sg, ex), _mm256_srli_epi32(v, 9)));
            a[k] = _mm256_fmadd_ps(u, c, a[k]);
            let rot = _mm256_or_si256(_mm256_slli_epi32(h[k], 1), _mm256_srli_epi32(h[k], 31));
            h[k] = _mm256_xor_si256(rot, _mm256_castps_si256(a[k]));
        }
    }
    for k in 0..V {
        _mm256_storeu_si256(s.r.as_mut_ptr().add(8 * k) as *mut __m256i, r[k]);
        _mm256_storeu_ps(s.a.as_mut_ptr().add(8 * k), a[k]);
        _mm256_storeu_si256(s.h.as_mut_ptr().add(8 * k) as *mut __m256i, h[k]);
    }
    fold(&s)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn run_avx512(mut s: State, iters: u64) -> u64 {
    use std::arch::x86_64::*;
    const V: usize = LANES / 16;
    let c = _mm512_set1_ps(C);
    let low4 = _mm512_set1_epi32(0xF);
    let exp0 = _mm512_set1_epi32(0x77);
    let sign = _mm512_set1_epi32(i32::MIN);
    let mut r = [_mm512_setzero_si512(); V];
    let mut a = [_mm512_setzero_ps(); V];
    let mut h = [_mm512_setzero_si512(); V];
    for k in 0..V {
        r[k] = _mm512_loadu_si512(s.r.as_ptr().add(16 * k) as *const __m512i);
        a[k] = _mm512_loadu_ps(s.a.as_ptr().add(16 * k));
        h[k] = _mm512_loadu_si512(s.h.as_ptr().add(16 * k) as *const __m512i);
    }
    for _ in 0..iters {
        for k in 0..V {
            let mut v = r[k];
            v = _mm512_xor_si512(v, _mm512_slli_epi32::<13>(v));
            v = _mm512_xor_si512(v, _mm512_srli_epi32::<17>(v));
            v = _mm512_xor_si512(v, _mm512_slli_epi32::<5>(v));
            r[k] = v;
            let sg = _mm512_and_si512(_mm512_slli_epi32::<27>(v), sign);
            let ex = _mm512_slli_epi32::<23>(_mm512_add_epi32(_mm512_and_si512(v, low4), exp0));
            let u = _mm512_castsi512_ps(_mm512_or_si512(_mm512_or_si512(sg, ex), _mm512_srli_epi32::<9>(v)));
            a[k] = _mm512_fmadd_ps(u, c, a[k]);
            h[k] = _mm512_xor_si512(_mm512_rol_epi32::<1>(h[k]), _mm512_castps_si512(a[k]));
        }
    }
    for k in 0..V {
        _mm512_storeu_si512(s.r.as_mut_ptr().add(16 * k) as *mut __m512i, r[k]);
        _mm512_storeu_ps(s.a.as_mut_ptr().add(16 * k), a[k]);
        _mm512_storeu_si512(s.h.as_mut_ptr().add(16 * k) as *mut __m512i, h[k]);
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
        assert_eq!(run(Isa::Scalar, 0, 1000, None), 0x44FC_675B_5443_6EDD);
        assert_eq!(run(Isa::Scalar, 1, 1000, None), 0x0859_DC6A_4B25_6B54);
        assert_eq!(run(Isa::Scalar, 42, 1000, None), 0x9F39_A25D_8600_DD1D);
    }

    #[test]
    fn simd_matches_scalar() {
        for isa in [Isa::Avx2, Isa::Avx512] {
            if !isa.supported() {
                eprintln!("fma32 {isa:?} 미지원 — 건너뜀");
                continue;
            }
            for seed in 0..32 {
                for iters in [1, 1000, 4097] {
                    assert_eq!(run(isa, seed, iters, None), run(Isa::Scalar, seed, iters, None), "{isa:?} seed={seed} iters={iters}");
                }
            }
        }
    }

    #[test]
    fn every_flip_changes_result() {
        let clean = run(Isa::Scalar, 7, 1000, None);
        for lane in 0..2 * LANES {
            for bit in [0, 1, 15, 16, 22, 23, 30, 31] {
                assert_ne!(run(Isa::Scalar, 7, 1000, Some(Flip { lane, bit })), clean, "lane={lane} bit={bit}");
            }
        }
    }

    // 변이 테스트 보강: 주입은 정확히 그 칸(정수 상태 0~31, 누적값 32~63)의 그 비트 하나만 뒤집어야 한다
    #[test]
    fn flip_targets_the_right_value() {
        let mut s = seed_state(7, None);
        s.r[3] ^= 1 << 20;
        assert_eq!(run(Isa::Scalar, 7, 1000, Some(Flip { lane: 3, bit: 20 })), run_scalar(s, 1000));
        let mut s = seed_state(7, None);
        s.a[5] = f32::from_bits(s.a[5].to_bits() ^ 1);
        s.h[5] = s.a[5].to_bits();
        assert_eq!(run(Isa::Scalar, 7, 1000, Some(Flip { lane: LANES + 5, bit: 0 })), run_scalar(s, 1000));
    }

    #[test]
    fn inputs_cover_exponents_and_signs() {
        let mut exps = std::collections::BTreeSet::new();
        let mut signs = std::collections::BTreeSet::new();
        for i in 0..2000u64 {
            let b = input_bits(splitmix64(i) as u32);
            exps.insert((b >> 23) & 0xFF);
            signs.insert(b >> 31);
        }
        assert_eq!(exps.len(), 16);
        assert_eq!((*exps.first().unwrap(), *exps.last().unwrap()), (0x77, 0x86));
        assert_eq!(signs.len(), 2);
    }

    #[test]
    fn accumulators_stay_finite_and_normal() {
        let mut s = seed_state(3, None);
        step_scalar(&mut s, 1 << 16);
        assert!(s.a.iter().all(|v| v.is_finite() && !v.is_subnormal()));
    }
}
