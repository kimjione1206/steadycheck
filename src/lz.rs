//! lz 커널: 압축 해제 모양. 64KiB 버퍼에 글자 그대로 넣기와 앞 내용 복사를 번갈아 하며
//! 쓴 바이트를 곧바로 메모리에서 다시 읽어 FNV 요약(h)에 넣는다.
//! 스칼라가 명세다 (지금은 모든 명령어 세트가 스칼라를 쓴다).

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

/// 검출 채점용: store(n) 이 true 면 n 번째로 쓰는 바이트를 바로 다음 자리에 쓴다. 평소엔 늘 false
pub(crate) fn run_scalar_faulty(seed: u64, iters: u64, flip: Option<Flip>, mut store: impl FnMut(u64) -> bool) -> u64 {
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
            // 글자 그대로: w0·w1 의 바이트를 작은 끝부터
            len = 1 + ((t >> 2) & 15) as usize;
            let w0 = t.rotate_left(17) ^ LIT_K0;
            let w1 = w0.rotate_left(29) ^ LIT_K1;
            let lit = (((w1 as u128) << 64) | w0 as u128).to_le_bytes();
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

/// 호출자(kernel::run_block)가 isa 지원 여부를 먼저 확인한다. 지금은 모든 isa 가 스칼라
pub fn run(_isa: Isa, seed: u64, iters: u64, flip: Option<Flip>) -> u64 {
    run_scalar_faulty(seed, iters, flip, |_| false)
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
}
