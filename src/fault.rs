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
}

#[derive(Clone, Copy, Debug)]
pub struct FaultInject {
    pub cpu: usize,
    pub block: u64,
    pub model: FaultModel,
}

const M1_EXP: u64 = 0x3EF;

/// 불량 코어가 이 블록을 계산했을 때 나올 요약값
pub fn run_faulty(model: FaultModel, kernel: Kernel, isa: Isa, seed: u64, iters: u64, threads: usize) -> u64 {
    match model {
        FaultModel::StartFlip(f) => run_block(kernel, isa, seed, iters, Some(f)),
        FaultModel::FewCoresOnly(f) => run_block(kernel, isa, seed, iters, (threads <= 2).then_some(f)),
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
        // FMA 를 쓰지 않는 계산 방식은 불량 장치를 지나지 않는다
        _ => run_block(kernel, isa, seed, iters, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_fma_kernels_pass_through_fma_faults() {
        let clean = run_block(Kernel::Wide, Isa::Scalar, 3, 1000, None);
        let m = FaultModel::FmaLaneEveryNth { lane: 0, n: 1, bit: 0 };
        assert_eq!(run_faulty(m, Kernel::Wide, Isa::Scalar, 3, 1000, 4), clean);
    }

    #[test]
    fn few_cores_threshold() {
        let f = Flip { lane: 1, bit: 2 };
        let clean = run_block(Kernel::Chain, Isa::Scalar, 3, 1000, None);
        assert_eq!(run_faulty(FaultModel::FewCoresOnly(f), Kernel::Chain, Isa::Scalar, 3, 1000, 3), clean);
        assert_ne!(run_faulty(FaultModel::FewCoresOnly(f), Kernel::Chain, Isa::Scalar, 3, 1000, 2), clean);
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
