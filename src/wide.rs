//! 넓은 정수 커널: chain 과 같은 계산을 32줄 동시에. 줄끼리 서로 기다리지 않아 실행 장치가 쉬지 않는다.

use crate::kernel::{splitmix64, Flip, Isa};

pub const LANES: usize = 32;
const MUL: u64 = 0x9E37_79B9;

fn seed_state(seed: u64) -> [u64; LANES] {
    let mut x = [0u64; LANES];
    for (i, v) in x.iter_mut().enumerate() {
        *v = splitmix64(seed.wrapping_mul(LANES as u64).wrapping_add(i as u64)) | 1;
    }
    x
}

fn fold(x: &[u64; LANES], y: &[u64; LANES]) -> u64 {
    let mut h = 0u64;
    for i in 0..LANES {
        h = h.rotate_left(7) ^ x[i] ^ y[i].rotate_left(29);
    }
    h
}

fn run_scalar(mut x: [u64; LANES], iters: u64) -> u64 {
    let mut y = [0u64; LANES];
    for _ in 0..iters {
        for i in 0..LANES {
            let mut v = x[i];
            v ^= v << 13;
            v ^= v >> 7;
            v ^= v << 17;
            x[i] = v;
            y[i] = y[i].wrapping_add((v & 0xFFFF_FFFF) * MUL);
        }
    }
    fold(&x, &y)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn run_avx2(x0: [u64; LANES], iters: u64) -> u64 {
    use std::arch::x86_64::*;
    const V: usize = LANES / 4;
    let m = _mm256_set1_epi64x(MUL as i64);
    let mut x = [_mm256_setzero_si256(); V];
    let mut y = [_mm256_setzero_si256(); V];
    for k in 0..V {
        x[k] = _mm256_loadu_si256(x0.as_ptr().add(4 * k) as *const __m256i);
    }
    for _ in 0..iters {
        for k in 0..V {
            let mut v = x[k];
            v = _mm256_xor_si256(v, _mm256_slli_epi64(v, 13));
            v = _mm256_xor_si256(v, _mm256_srli_epi64(v, 7));
            v = _mm256_xor_si256(v, _mm256_slli_epi64(v, 17));
            x[k] = v;
            y[k] = _mm256_add_epi64(y[k], _mm256_mul_epu32(v, m));
        }
    }
    let mut ox = [0u64; LANES];
    let mut oy = [0u64; LANES];
    for k in 0..V {
        _mm256_storeu_si256(ox.as_mut_ptr().add(4 * k) as *mut __m256i, x[k]);
        _mm256_storeu_si256(oy.as_mut_ptr().add(4 * k) as *mut __m256i, y[k]);
    }
    fold(&ox, &oy)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn run_avx512(x0: [u64; LANES], iters: u64) -> u64 {
    use std::arch::x86_64::*;
    const V: usize = LANES / 8;
    let m = _mm512_set1_epi64(MUL as i64);
    let mut x = [_mm512_setzero_si512(); V];
    let mut y = [_mm512_setzero_si512(); V];
    for k in 0..V {
        x[k] = _mm512_loadu_si512(x0.as_ptr().add(8 * k) as *const __m512i);
    }
    for _ in 0..iters {
        for k in 0..V {
            let mut v = x[k];
            v = _mm512_xor_si512(v, _mm512_slli_epi64::<13>(v));
            v = _mm512_xor_si512(v, _mm512_srli_epi64::<7>(v));
            v = _mm512_xor_si512(v, _mm512_slli_epi64::<17>(v));
            x[k] = v;
            y[k] = _mm512_add_epi64(y[k], _mm512_mul_epu32(v, m));
        }
    }
    let mut ox = [0u64; LANES];
    let mut oy = [0u64; LANES];
    for k in 0..V {
        _mm512_storeu_si512(ox.as_mut_ptr().add(8 * k) as *mut __m512i, x[k]);
        _mm512_storeu_si512(oy.as_mut_ptr().add(8 * k) as *mut __m512i, y[k]);
    }
    fold(&ox, &oy)
}

/// 호출자(kernel::run_block)가 isa 지원 여부를 먼저 확인한다.
pub fn run(isa: Isa, seed: u64, iters: u64, flip: Option<Flip>) -> u64 {
    let mut x = seed_state(seed);
    if let Some(f) = flip {
        x[f.lane % LANES] ^= 1u64 << (f.bit % 64);
    }
    match isa {
        Isa::Scalar => run_scalar(x, iters),
        #[cfg(target_arch = "x86_64")]
        Isa::Avx2 => unsafe { run_avx2(x, iters) },
        #[cfg(target_arch = "x86_64")]
        Isa::Avx512 => unsafe { run_avx512(x, iters) },
        #[allow(unreachable_patterns)]
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 정답값: 2026-09-30 맥 ARM·Rosetta x86 스칼라에서 같은 값으로 실측
    #[test]
    fn known_answers() {
        assert_eq!(run(Isa::Scalar, 0, 1000, None), 0x7FF7_52D2_0C7F_DE0E);
        assert_eq!(run(Isa::Scalar, 1, 1000, None), 0xF6CB_5D24_D334_4B67);
        assert_eq!(run(Isa::Scalar, 42, 1000, None), 0xCCB8_B10A_46AD_EC88);
    }

    #[test]
    fn simd_matches_scalar() {
        for isa in [Isa::Avx2, Isa::Avx512] {
            if !isa.supported() {
                eprintln!("wide {isa:?} 미지원 — 건너뜀");
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
        for lane in 0..LANES {
            for bit in [0, 1, 31, 32, 63] {
                assert_ne!(run(Isa::Scalar, 7, 1000, Some(Flip { lane, bit })), clean, "lane={lane} bit={bit}");
            }
        }
    }

    #[test]
    fn flip_changes_exactly_one_bit() {
        let mut x = seed_state(7);
        x[20] ^= 1 << 40;
        assert_eq!(run(Isa::Scalar, 7, 1000, Some(Flip { lane: 20, bit: 40 })), run_scalar(x, 1000));
    }
}
