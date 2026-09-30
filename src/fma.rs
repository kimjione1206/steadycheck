//! FMA 커널: 곱셈-덧셈 장치(FMA)를 계속 돌린다.
//! 입력은 부호·지수까지 무작위(2^-16 ~ 2^15) — 대규모 조사에서 FMA 불량이 입력 지수 비트에 치우쳤다.
//! 입력 하나로 누적값 4벌에 FMA 를 4번 해 장치를 빽빽하게 채운다.
//! 매 반복의 누적값 비트를 정수 요약(h)에 회전-XOR 로 새겨, 한 번 틀린 결과가 반올림으로 사라지지 않게 한다.

use crate::kernel::{splitmix64, Flip, Isa};

pub const LANES: usize = 32;
const K: usize = 4;
const C: [f64; K] = [1.5, -1.25, 0.75, -1.75];
const IN_MASK: u64 = 0x81FF_FFFF_FFFF_FFFF;
const IN_EXP: u64 = 0x3EF << 52;

/// 난수 → 실수 비트(2연산): 부호 = 비트 63, 지수 = 0x3EF + 비트 52~56 (2^-16 ~ 2^15), 가수 = 비트 0~51
#[inline(always)]
fn input_bits(v: u64) -> u64 {
    (v & IN_MASK) + IN_EXP
}

struct State {
    r: [u64; LANES],
    a: [[f64; LANES]; K],
    h: [u64; LANES],
}

// 줄 i 의 누적값 4벌을 한 값으로 섞는다
#[inline(always)]
fn trace(a: &[[f64; LANES]; K], i: usize) -> u64 {
    a[0][i].to_bits() ^ a[1][i].to_bits().rotate_left(16) ^ a[2][i].to_bits().rotate_left(32) ^ a[3][i].to_bits().rotate_left(48)
}

// 시작 누적값도 h 에 새겨 둔다: 뒤집힌 낮은 비트가 첫 반올림에 사라져도 흔적이 남는다
fn start_trace(a: &[[f64; LANES]; K]) -> [u64; LANES] {
    let mut h = [0u64; LANES];
    for i in 0..LANES {
        h[i] = trace(a, i);
    }
    h
}

fn seed_state(seed: u64, flip: Option<Flip>) -> State {
    let mut r = [0u64; LANES];
    let mut a = [[0f64; LANES]; K];
    for i in 0..LANES {
        r[i] = splitmix64(seed.wrapping_mul(LANES as u64).wrapping_add(i as u64)) | 1;
        for k in 0..K {
            a[k][i] = f64::from_bits(input_bits(r[i].rotate_left(16 * (k as u32 + 1))));
        }
    }
    if let Some(f) = flip {
        let lane = f.lane % (2 * LANES);
        let bit = 1u64 << (f.bit % 64);
        if lane < LANES {
            r[lane] ^= bit;
        } else {
            a[0][lane - LANES] = f64::from_bits(a[0][lane - LANES].to_bits() ^ bit);
        }
    }
    let h = start_trace(&a);
    State { r, a, h }
}

fn fold(s: &State) -> u64 {
    let mut x = 0u64;
    for i in 0..LANES {
        x = x.rotate_left(7)
            ^ s.r[i]
            ^ s.a[0][i].to_bits().rotate_left(17)
            ^ s.a[1][i].to_bits().rotate_left(23)
            ^ s.a[2][i].to_bits().rotate_left(37)
            ^ s.a[3][i].to_bits().rotate_left(43)
            ^ s.h[i].rotate_left(29);
    }
    x
}

/// fault: (줄, 반복 번호, 입력 비트) → 첫 누적값(a0) 결과 비트에 XOR 할 값. 평소엔 0 — 검출 채점용 불량 흉내 자리
fn step_scalar_with(s: &mut State, iters: u64, mut fault: impl FnMut(usize, u64, u64) -> u64) {
    for n in 0..iters {
        for i in 0..LANES {
            let mut v = s.r[i];
            v ^= v << 13;
            v ^= v >> 7;
            v ^= v << 17;
            s.r[i] = v;
            let ub = input_bits(v);
            let u = f64::from_bits(ub);
            // mul_add 는 한 번만 반올림 — 하드웨어 FMA 와 비트 단위로 같다
            for k in 0..K {
                s.a[k][i] = u.mul_add(C[k], s.a[k][i]);
            }
            s.a[0][i] = f64::from_bits(s.a[0][i].to_bits() ^ fault(i, n, ub));
            s.h[i] = s.h[i].rotate_left(1) ^ trace(&s.a, i);
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

// AVX2 는 계산용 임시 칸이 16개뿐이라 32줄을 8줄씩 네 번 계산한다 (줄끼리 독립이라 결과 동일)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn run_avx2(mut s: State, iters: u64) -> u64 {
    use std::arch::x86_64::*;
    const V: usize = 2;
    let c = [_mm256_set1_pd(C[0]), _mm256_set1_pd(C[1]), _mm256_set1_pd(C[2]), _mm256_set1_pd(C[3])];
    let mask = _mm256_set1_epi64x(IN_MASK as i64);
    let exp = _mm256_set1_epi64x(IN_EXP as i64);
    // 64비트 칸 안에서 바이트 자리 바꾸기 = 16비트·48비트 회전
    let rot16 = _mm256_setr_epi8(6, 7, 0, 1, 2, 3, 4, 5, 14, 15, 8, 9, 10, 11, 12, 13, 6, 7, 0, 1, 2, 3, 4, 5, 14, 15, 8, 9, 10, 11, 12, 13);
    let rot48 = _mm256_setr_epi8(2, 3, 4, 5, 6, 7, 0, 1, 10, 11, 12, 13, 14, 15, 8, 9, 2, 3, 4, 5, 6, 7, 0, 1, 10, 11, 12, 13, 14, 15, 8, 9);
    for part in 0..LANES / 8 {
        let o = part * 8;
        let mut r = [_mm256_setzero_si256(); V];
        let mut a = [[_mm256_setzero_pd(); V]; K];
        let mut h = [_mm256_setzero_si256(); V];
        for j in 0..V {
            r[j] = _mm256_loadu_si256(s.r.as_ptr().add(o + 4 * j) as *const __m256i);
            for k in 0..K {
                a[k][j] = _mm256_loadu_pd(s.a[k].as_ptr().add(o + 4 * j));
            }
            h[j] = _mm256_loadu_si256(s.h.as_ptr().add(o + 4 * j) as *const __m256i);
        }
        for _ in 0..iters {
            for j in 0..V {
                let mut v = r[j];
                v = _mm256_xor_si256(v, _mm256_slli_epi64(v, 13));
                v = _mm256_xor_si256(v, _mm256_srli_epi64(v, 7));
                v = _mm256_xor_si256(v, _mm256_slli_epi64(v, 17));
                r[j] = v;
                let u = _mm256_castsi256_pd(_mm256_add_epi64(_mm256_and_si256(v, mask), exp));
                for k in 0..K {
                    a[k][j] = _mm256_fmadd_pd(u, c[k], a[k][j]);
                }
                let t0 = _mm256_xor_si256(_mm256_castpd_si256(a[0][j]), _mm256_shuffle_epi8(_mm256_castpd_si256(a[1][j]), rot16));
                let t1 = _mm256_xor_si256(
                    _mm256_shuffle_epi32(_mm256_castpd_si256(a[2][j]), 0xB1),
                    _mm256_shuffle_epi8(_mm256_castpd_si256(a[3][j]), rot48),
                );
                let rot = _mm256_or_si256(_mm256_slli_epi64(h[j], 1), _mm256_srli_epi64(h[j], 63));
                h[j] = _mm256_xor_si256(rot, _mm256_xor_si256(t0, t1));
            }
        }
        for j in 0..V {
            _mm256_storeu_si256(s.r.as_mut_ptr().add(o + 4 * j) as *mut __m256i, r[j]);
            for k in 0..K {
                _mm256_storeu_pd(s.a[k].as_mut_ptr().add(o + 4 * j), a[k][j]);
            }
            _mm256_storeu_si256(s.h.as_mut_ptr().add(o + 4 * j) as *mut __m256i, h[j]);
        }
    }
    fold(&s)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn run_avx512(mut s: State, iters: u64) -> u64 {
    use std::arch::x86_64::*;
    const V: usize = LANES / 8;
    let c = [_mm512_set1_pd(C[0]), _mm512_set1_pd(C[1]), _mm512_set1_pd(C[2]), _mm512_set1_pd(C[3])];
    let mask = _mm512_set1_epi64(IN_MASK as i64);
    let exp = _mm512_set1_epi64(IN_EXP as i64);
    let mut r = [_mm512_setzero_si512(); V];
    let mut a = [[_mm512_setzero_pd(); V]; K];
    let mut h = [_mm512_setzero_si512(); V];
    for j in 0..V {
        r[j] = _mm512_loadu_si512(s.r.as_ptr().add(8 * j) as *const __m512i);
        for k in 0..K {
            a[k][j] = _mm512_loadu_pd(s.a[k].as_ptr().add(8 * j));
        }
        h[j] = _mm512_loadu_si512(s.h.as_ptr().add(8 * j) as *const __m512i);
    }
    for _ in 0..iters {
        for j in 0..V {
            let mut v = r[j];
            v = _mm512_xor_si512(v, _mm512_slli_epi64::<13>(v));
            v = _mm512_xor_si512(v, _mm512_srli_epi64::<7>(v));
            v = _mm512_xor_si512(v, _mm512_slli_epi64::<17>(v));
            r[j] = v;
            let u = _mm512_castsi512_pd(_mm512_add_epi64(_mm512_and_si512(v, mask), exp));
            for k in 0..K {
                a[k][j] = _mm512_fmadd_pd(u, c[k], a[k][j]);
            }
            let t0 = _mm512_xor_si512(_mm512_castpd_si512(a[0][j]), _mm512_rol_epi64::<16>(_mm512_castpd_si512(a[1][j])));
            let t1 = _mm512_xor_si512(
                _mm512_rol_epi64::<32>(_mm512_castpd_si512(a[2][j])),
                _mm512_rol_epi64::<48>(_mm512_castpd_si512(a[3][j])),
            );
            h[j] = _mm512_xor_si512(_mm512_rol_epi64::<1>(h[j]), _mm512_xor_si512(t0, t1));
        }
    }
    for j in 0..V {
        _mm512_storeu_si512(s.r.as_mut_ptr().add(8 * j) as *mut __m512i, r[j]);
        for k in 0..K {
            _mm512_storeu_pd(s.a[k].as_mut_ptr().add(8 * j), a[k][j]);
        }
        _mm512_storeu_si512(s.h.as_mut_ptr().add(8 * j) as *mut __m512i, h[j]);
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

    // 정답값: 2026-09-30 맥 ARM 스칼라로 실측 (CI x86 스칼라·AVX2·AVX-512 에서 같은 값인지 확인)
    #[test]
    fn known_answers() {
        assert_eq!(run(Isa::Scalar, 0, 1000, None), 0x947C_1716_0C7C_249F);
        assert_eq!(run(Isa::Scalar, 1, 1000, None), 0x9B49_7925_C65E_CE63);
        assert_eq!(run(Isa::Scalar, 42, 1000, None), 0xC486_3A6B_5E8E_D816);
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

    // 정수 상태(0~31)와 첫 누적값(32~63) 어느 쪽의 어느 비트를 뒤집어도 결과가 달라져야 한다
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
        s.a[0][5] = f64::from_bits(s.a[0][5].to_bits() ^ 1);
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

    // 누적값 4벌 모두 오래 돌아도 유한하고 비정규수가 아니어야 한다
    #[test]
    fn accumulators_stay_finite_and_normal() {
        let mut s = seed_state(3, None);
        step_scalar(&mut s, 1 << 16);
        assert!(s.a.iter().flatten().all(|v| v.is_finite() && !v.is_subnormal()));
    }

    #[test]
    fn fault_hook_zero_is_identity() {
        for seed in [0, 5] {
            assert_eq!(run_scalar_faulty(seed, 1000, |_, _, _| 0), run(Isa::Scalar, seed, 1000, None));
        }
        assert_ne!(run_scalar_faulty(5, 1000, |l, n, _| if l == 3 && n == 10 { 1 } else { 0 }), run(Isa::Scalar, 5, 1000, None));
    }
}
