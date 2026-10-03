//! lz 커널: 압축 해제 모양. 64KiB 버퍼에 글자 그대로 넣기와 앞 내용 복사를 번갈아 하며
//! 쓴 바이트를 곧바로 메모리에서 다시 읽어 FNV 요약(h)에 넣는다.
//! 스칼라가 명세다. AVX2·AVX-512 PC 는 BMI 비트 꺼내기·rep movsb 를 쓰는 빠른 경로로 같은 바이트·결과를 낸다.

use crate::kernel::{splitmix64, Flip, Isa};

const BUF: usize = 1 << 16;
const M: usize = BUF - 1;
const FNV_OFF: u64 = 0xCBF2_9CE4_8422_2325;
const FNV_P: u64 = 0x0000_0100_0000_01B3;
const DIST_MASK: u64 = 0x5555_0000_AAAA_0000;
const LIT_K0: u64 = 0xA076_1D64_78BD_642F;
const LIT_K1: u64 = 0xE703_7ED1_A0B4_28DB;

/// mask 의 1 비트 자리(낮은 쪽부터)에 있는 v 의 비트를 아래부터 차례로 모은다
fn pext(v: u64, mask: u64) -> u64 {
    let mut r = 0;
    let mut k = 0;
    for b in 0..64 {
        if (mask >> b) & 1 == 1 {
            r |= ((v >> b) & 1) << k;
            k += 1;
        }
    }
    r
}

/// 시작 상태 (난수 x, 요약 h). 뒤집기: 짝수 lane → x, 홀수 lane → h
fn start(seed: u64, flip: Option<Flip>) -> (u64, u64) {
    let mut x = splitmix64(seed) | 1;
    let mut h = FNV_OFF ^ seed;
    if let Some(f) = flip {
        if f.lane % 2 == 0 {
            x ^= 1u64 << (f.bit % 64);
        } else {
            h ^= 1u64 << (f.bit % 64);
        }
        if x == 0 {
            x = 0x9E37_79B9_7F4A_7C15;
        }
    }
    (x, h)
}

/// 글자 그대로 넣을 16바이트: w0·w1 의 작은 끝부터
#[inline(always)]
fn literal(t: u64) -> [u8; 16] {
    let w0 = t.rotate_left(17) ^ LIT_K0;
    let w1 = w0.rotate_left(29) ^ LIT_K1;
    (((w1 as u128) << 64) | w0 as u128).to_le_bytes()
}

/// 검출 채점용: store(n) 이 true 면 n 번째로 쓰는 바이트를 바로 다음 자리에 쓴다. 평소엔 늘 false
pub(crate) fn run_scalar_faulty(seed: u64, iters: u64, flip: Option<Flip>, mut store: impl FnMut(u64) -> bool) -> u64 {
    let (mut x, mut h) = start(seed, flip);
    let mut out = vec![0u8; BUF];
    let mut p = 0usize;
    let mut n = 0u64;
    let mut put = |out: &mut [u8], at: usize, b: u8| {
        let at = if store(n) { at + 1 } else { at };
        out[at & M] = b;
        n += 1;
    };
    for _ in 0..iters {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let t = x;
        let len;
        if t & 2 == 0 {
            len = 1 + ((t >> 2) & 15) as usize;
            let lit = literal(t);
            for i in 0..len {
                put(&mut out, p + i, lit[i]);
            }
        } else {
            // 앞 내용 복사: 겹치면 방금 쓴 바이트를 다시 읽는다
            len = 3 + ((t >> 2) & 63) as usize;
            let dist = 1 + (pext(t, DIST_MASK) & 0x3FFF) as usize;
            for i in 0..len {
                let b = out[(p + i + BUF - dist) & M];
                put(&mut out, p + i, b);
            }
        }
        // 쓴 바이트를 메모리에서 다시 읽어 요약
        for i in 0..len {
            h = (h ^ out[(p + i) & M] as u64).wrapping_mul(FNV_P);
        }
        p = (p + len) & M;
    }
    h
}

// 빠른 경로: 명세와 바이트·결과가 같다. 다른 점은 비트 꺼내기(bextr·pext)와 한 번에 쓰는 복사뿐
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "bmi1,bmi2")]
unsafe fn run_fast(seed: u64, iters: u64, flip: Option<Flip>) -> u64 {
    use std::arch::x86_64::{_bextr_u64, _pext_u64};
    let (mut x, mut h) = start(seed, flip);
    let mut out = vec![0u8; BUF];
    // 모든 읽기·쓰기를 이 포인터 하나로 (자리는 늘 & M 이나 버퍼 끝 확인으로 버퍼 안)
    let buf = out.as_mut_ptr();
    let mut p = 0usize;
    for _ in 0..iters {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let t = x;
        let len;
        if t & 2 == 0 {
            len = 1 + _bextr_u64(t, 2, 4) as usize;
            let lit = literal(t);
            if p + len <= BUF {
                // 정확히 len 바이트만 쓴다
                std::ptr::copy_nonoverlapping(lit.as_ptr(), buf.add(p), len);
            } else {
                for i in 0..len {
                    *buf.add((p + i) & M) = lit[i];
                }
            }
        } else {
            len = 3 + _bextr_u64(t, 2, 6) as usize;
            let dist = 1 + (_pext_u64(t, DIST_MASK) & 0x3FFF) as usize;
            let src = (p + BUF - dist) & M;
            if dist >= len && src + len <= BUF && p + len <= BUF {
                // 겹치지 않고 버퍼 끝도 안 넘음 → 한 번에 복사 (방향 플래그는 Rust 규약상 0 = 앞으로)
                std::arch::asm!(
                    "rep movsb",
                    inout("rcx") len => _,
                    inout("rsi") buf.add(src) as *const u8 => _,
                    inout("rdi") buf.add(p) => _,
                    options(nostack, preserves_flags)
                );
            } else {
                for i in 0..len {
                    *buf.add((p + i) & M) = *buf.add((p + i + BUF - dist) & M);
                }
            }
        }
        for i in 0..len {
            h = (h ^ *buf.add((p + i) & M) as u64).wrapping_mul(FNV_P);
        }
        p = (p + len) & M;
    }
    h
}

/// 호출자(kernel::run_block)가 isa 지원 여부를 먼저 확인한다.
pub fn run(isa: Isa, seed: u64, iters: u64, flip: Option<Flip>) -> u64 {
    match isa {
        Isa::Scalar => run_scalar_faulty(seed, iters, flip, |_| false),
        #[cfg(target_arch = "x86_64")]
        Isa::Avx2 | Isa::Avx512 => unsafe { run_fast(seed, iters, flip) },
        #[allow(unreachable_patterns)]
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 정답값: 명세를 따로 옮긴 파이썬 구현으로 계산
    #[test]
    fn known_answers() {
        assert_eq!(run(Isa::Scalar, 0, 1000, None), 0x147B_51B8_482F_3284);
        assert_eq!(run(Isa::Scalar, 5, 4096, None), 0x9800_1D50_B7C8_B7FD);
        assert_eq!(run(Isa::Scalar, 3, 1000, Some(Flip { lane: 0, bit: 5 })), 0x22FD_1E36_B8D7_C45B);
        assert_eq!(run(Isa::Scalar, 3, 1000, Some(Flip { lane: 1, bit: 40 })), 0x6CF0_7A46_8A11_A10B);
    }

    #[test]
    fn simd_matches_scalar() {
        for isa in [Isa::Avx2, Isa::Avx512] {
            if !isa.supported() {
                eprintln!("lz {isa:?} 미지원 — 건너뜀");
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
    fn pext_known() {
        assert_eq!(pext(0x1234_5678_9ABC_DEF0, DIST_MASK), 0x46BE);
    }

    // 난수 상태(짝수 lane)와 요약(홀수 lane) 어느 쪽의 어느 비트를 뒤집어도 결과가 달라져야 한다
    #[test]
    fn every_flip_changes_result() {
        let clean = run(Isa::Scalar, 7, 1000, None);
        for lane in 0..4 {
            for bit in [0, 1, 31, 32, 62, 63] {
                assert_ne!(run(Isa::Scalar, 7, 1000, Some(Flip { lane, bit })), clean, "lane={lane} bit={bit}");
            }
        }
    }

    #[test]
    fn faulty_hook_never_is_identity() {
        for seed in [0, 5] {
            assert_eq!(run_scalar_faulty(seed, 1000, None, |_| false), run(Isa::Scalar, seed, 1000, None));
        }
    }

    #[test]
    fn faulty_hook_shifts_byte() {
        assert_ne!(run_scalar_faulty(5, 1000, None, |n| n == 0), run(Isa::Scalar, 5, 1000, None));
    }

    // 정답값(파이썬 구현): 70131 은 복사 묶음의 마지막 바이트(밀린 바이트가 덮이지 않음), 70133 은 글자 그대로 묶음 안
    #[test]
    fn faulty_hook_known_answers() {
        assert_eq!(run_scalar_faulty(5, 4096, None, |n| n == 70131), 0x972B_E044_3AFF_BA84);
        assert_eq!(run_scalar_faulty(5, 4096, None, |n| n == 70133), 0x96CE_2146_3087_D508);
    }

    // splitmix64(이 시드) == 1 이라 lane 0 bit 0 뒤집기로 x == 0 이 된다 → 대체값으로 시작
    #[test]
    fn zero_state_is_replaced() {
        const SEED: u64 = 0xF836_4607_E9C9_49BD;
        assert_eq!(splitmix64(SEED), 1);
        assert_eq!(run(Isa::Scalar, SEED, 1000, Some(Flip { lane: 0, bit: 0 })), 0xBC16_CAEF_B648_B47A);
    }
}
