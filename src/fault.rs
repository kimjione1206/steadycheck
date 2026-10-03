//! 검출 능력 채점 전용: 실제 불량 사례를 흉내 낸 오류 모델.
//! 지정한 코어·블록에서 "불량 코어가 냈을 값"을 스칼라로 만든다 (스칼라와 SIMD 는 비트 단위로 같으므로 동등).

use crate::kernel::{run_block, Flip, Isa, Kernel};

#[derive(Clone, Copy, Debug)]
pub enum FaultModel {
    /// 블록 시작 때 상태 비트 하나 (기존 주입과 같음)
    StartFlip(Flip),
    /// FMA 입력 지수가 0x3EF(가장 작은 값)일 때만 처음 한 번 결과 가수 비트가 틀린다 — 대규모 조사에서 FMA 불량은 입력 지수 비트에 치우침
    FmaExpConditional { bit: u32 },
    /// FMA 줄 하나에서 n 번째 연산마다 결과 가수 비트가 틀린다 — 불량은 대개 벡터 줄 하나에만 있음
    FmaLaneEveryNth { lane: usize, n: u64, bit: u32 },
    /// 동시에 도는 스레드가 2개 이하일 때만 틀린다 — 한두 코어가 최고 클럭일 때만 불안정한 CPU
    FewCoresOnly(Flip),
    /// 잠들었다 깨어난 뒤 첫 블록에서만 틀린다 — 쉬다가 부하를 받는 순간 불안정한 CPU
    AfterWake(Flip),
    /// lz 에서 쓴 바이트 순번 n 이 (n + 1) % every == 0 이면 그 바이트가 바로 다음 자리에 저장된다 — 압축 해제처럼 바이트를 옮기는 일에서만 드러나는 저장 불량. every 0 이면 꺼짐
    ByteNeighborStore { every: u64 },
}

#[derive(Clone, Copy, Debug)]
pub struct FaultInject {
    pub cpu: usize,
    pub block: u64,
    pub model: FaultModel,
}

impl FaultInject {
    /// 이 블록에 불량을 넣을지
    pub fn fires_at(&self, block: u64, just_woke: bool) -> bool {
        match self.model {
            FaultModel::AfterWake(_) => just_woke && block >= self.block,
            _ => block == self.block,
        }
    }
}

const M1_EXP: u64 = 0x3EF;

/// 불량 코어가 이 블록을 계산했을 때 나올 요약값
/// active: 지금 동시에 계산 중인 워커 수
pub fn run_faulty(model: FaultModel, kernel: Kernel, isa: Isa, seed: u64, iters: u64, active: usize) -> u64 {
    match model {
        FaultModel::StartFlip(f) => run_block(kernel, isa, seed, iters, Some(f)),
        FaultModel::FewCoresOnly(f) => run_block(kernel, isa, seed, iters, (active <= 2).then_some(f)),
        FaultModel::AfterWake(f) => run_block(kernel, isa, seed, iters, Some(f)),
        FaultModel::FmaExpConditional { bit } if kernel == Kernel::Fma => {
            let mut fired = false;
            crate::fma::run_scalar_faulty(seed, iters, |_, _, u_bits| {
                if !fired && (u_bits >> 52) & 0x7FF == M1_EXP {
                    fired = true;
                    1u64 << (bit % 52)
                } else {
                    0
                }
            })
        }
        FaultModel::FmaLaneEveryNth { lane, n, bit } if kernel == Kernel::Fma => {
            crate::fma::run_scalar_faulty(seed, iters, |l, op, _| {
                if l == lane % crate::fma::LANES && n > 0 && (op + 1) % n == 0 { 1u64 << (bit % 52) } else { 0 }
            })
        }
        FaultModel::ByteNeighborStore { every } if kernel == Kernel::Lz => {
            crate::lz::run_scalar_faulty(seed, iters, None, |n| every > 0 && (n + 1) % every == 0)
        }
        // FMA 불량 모델은 배정밀도 fma 커널만, 바이트 저장 불량은 lz 커널만 흉내 낸다. 다른 커널(FMA 를 쓰는 fma32 포함)은
        // 깨끗하게 통과시키며, 지금은 이 모델들의 범위 밖이다
        _ => run_block(kernel, isa, seed, iters, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_fma_kernels_pass_through_fma_faults() {
        let clean = run_block(Kernel::Wide, Isa::Scalar, 3, 1000, None);
        for m in [FaultModel::FmaLaneEveryNth { lane: 0, n: 1, bit: 0 }, FaultModel::FmaExpConditional { bit: 0 }] {
            assert_eq!(run_faulty(m, Kernel::Wide, Isa::Scalar, 3, 1000, 4), clean, "{m:?}");
        }
    }

    // 변이 테스트 보강: M1 은 입력 지수가 0x3EF 인 첫 연산 한 번만, 가수 비트 (bit % 52) 를 뒤집는다
    #[test]
    fn exp_conditional_fires_once_at_first_match() {
        let mut first = None;
        crate::fma::run_scalar_faulty(3, 1000, |l, n, u| {
            if first.is_none() && (u >> 52) & 0x7FF == 0x3EF {
                first = Some((l, n));
            }
            0
        });
        let first = first.expect("1000 반복 안에 가장 작은 지수가 나와야 한다");
        let want = crate::fma::run_scalar_faulty(3, 1000, |l, n, _| if (l, n) == first { 1 << 8 } else { 0 });
        assert_eq!(run_faulty(FaultModel::FmaExpConditional { bit: 60 }, Kernel::Fma, Isa::Scalar, 3, 1000, 4), want);
    }

    // 변이 테스트 보강: M2 는 줄 (lane % 32) 의 n 번째·2n 번째… 연산에서 가수 비트 (bit % 52) 를 뒤집고, n = 0 이면 꺼진다
    #[test]
    fn lane_every_nth_hits_exact_ops() {
        let m = FaultModel::FmaLaneEveryNth { lane: 37, n: 100, bit: 60 };
        let want = crate::fma::run_scalar_faulty(3, 1000, |l, op, _| if l == 5 && op % 100 == 99 { 1 << 8 } else { 0 });
        assert_eq!(run_faulty(m, Kernel::Fma, Isa::Scalar, 3, 1000, 4), want);
        let clean = run_block(Kernel::Fma, Isa::Scalar, 3, 1000, None);
        let off = FaultModel::FmaLaneEveryNth { lane: 5, n: 0, bit: 10 };
        assert_eq!(run_faulty(off, Kernel::Fma, Isa::Scalar, 3, 1000, 4), clean);
    }

    #[test]
    fn byte_neighbor_only_affects_lz() {
        let m = FaultModel::ByteNeighborStore { every: 1000 };
        for k in [Kernel::Chain, Kernel::Wide, Kernel::Fma] {
            assert_eq!(run_faulty(m, k, Isa::Scalar, 3, 4096, 4), run_block(k, Isa::Scalar, 3, 4096, None), "{k:?}");
        }
        assert_ne!(run_faulty(m, Kernel::Lz, Isa::Scalar, 3, 4096, 4), run_block(Kernel::Lz, Isa::Scalar, 3, 4096, None));
    }

    #[test]
    fn byte_neighbor_every_zero_is_off() {
        let m = FaultModel::ByteNeighborStore { every: 0 };
        assert_eq!(run_faulty(m, Kernel::Lz, Isa::Scalar, 3, 4096, 4), run_block(Kernel::Lz, Isa::Scalar, 3, 4096, None));
    }

    #[test]
    fn few_cores_threshold() {
        let f = Flip { lane: 1, bit: 2 };
        let clean = run_block(Kernel::Chain, Isa::Scalar, 3, 1000, None);
        assert_eq!(run_faulty(FaultModel::FewCoresOnly(f), Kernel::Chain, Isa::Scalar, 3, 1000, 3), clean);
        assert_ne!(run_faulty(FaultModel::FewCoresOnly(f), Kernel::Chain, Isa::Scalar, 3, 1000, 2), clean);
    }

    #[test]
    fn fires_at_rules() {
        let f = |model| FaultInject { cpu: 0, block: 3, model };
        let flip = Flip { lane: 1, bit: 2 };
        assert!(f(FaultModel::StartFlip(flip)).fires_at(3, false));
        assert!(!f(FaultModel::StartFlip(flip)).fires_at(4, true));
        assert!(!f(FaultModel::AfterWake(flip)).fires_at(3, false));
        assert!(!f(FaultModel::AfterWake(flip)).fires_at(2, true));
        assert!(f(FaultModel::AfterWake(flip)).fires_at(3, true));
        assert!(f(FaultModel::AfterWake(flip)).fires_at(9, true));
    }

    #[test]
    fn lane_every_nth_fires() {
        let clean = run_block(Kernel::Fma, Isa::Scalar, 3, 1000, None);
        let m = FaultModel::FmaLaneEveryNth { lane: 5, n: 100, bit: 10 };
        assert_ne!(run_faulty(m, Kernel::Fma, Isa::Scalar, 3, 1000, 4), clean);
        // n 이 반복 수보다 크면 한 번도 안 켜진다
        let m = FaultModel::FmaLaneEveryNth { lane: 5, n: 5000, bit: 10 };
        assert_eq!(run_faulty(m, Kernel::Fma, Isa::Scalar, 3, 1000, 4), clean);
    }
}
