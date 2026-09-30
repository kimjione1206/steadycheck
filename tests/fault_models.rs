//! 실제 불량 사례를 흉내 낸 모델로 검출 능력을 잰다.
//! 빈틈 모델은 지금 "안 잡힘"을 단언한다 — 빈틈을 메우는 작업에서 "잡힘"으로 바꾼다.

use std::time::Duration;
use steadycheck::cpu::{self, CpuConfig, KernelSet, Pattern};
use steadycheck::fault::{FaultInject, FaultModel};
use steadycheck::kernel::{Flip, Isa};

fn run(kernels: KernelSet, pattern: Pattern, threads: usize, model: FaultModel) -> bool {
    let out = cpu::run(&CpuConfig {
        isa: Isa::best(), threads, duration: Duration::from_secs(10), kernels, pattern,
        iters: Some(1 << 12), inject: None, rotate_isa: false, fault: Some(FaultInject { cpu: 0, block: 2, model }),
    });
    if let Some(e) = &out.error {
        assert_eq!((e.cpu, e.block), (0, 2), "엉뚱한 곳에서 검출: {e:?}");
    }
    out.error.is_some()
}

fn caught_at(kernels: KernelSet, pattern: Pattern, threads: usize, model: FaultModel) -> Option<cpu::CpuError> {
    cpu::run(&CpuConfig {
        isa: Isa::best(), threads, duration: Duration::from_secs(10), kernels, pattern,
        iters: Some(1 << 12), inject: None, rotate_isa: false, fault: Some(FaultInject { cpu: 0, block: 0, model }),
    })
    .error
}

#[test]
fn m1_fma_exponent_conditional() {
    // 입력을 넓힌 뒤: 지수 조건 불량이 켜지고 잡힌다
    assert!(run(KernelSet::Fma, Pattern::Steady, 2, FaultModel::FmaExpConditional { bit: 20 }), "M1 을 못 잡음");
    eprintln!("M1 FMA 입력 지수 조건: 잡힘");
}

#[test]
fn m2_fma_lane_every_nth() {
    let model = FaultModel::FmaLaneEveryNth { lane: 7, n: 1000, bit: 20 };
    assert!(run(KernelSet::Fma, Pattern::Steady, 2, model), "M2 를 fma 로 못 잡음");
    // chain 은 FMA 장치를 쓰지 않으므로 이 불량을 지나지 않는다
    assert!(!run(KernelSet::Chain, Pattern::Steady, 2, model), "chain 이 FMA 불량을 잡았다 — 흉내가 잘못됨");
    eprintln!("M2 FMA 줄 하나 N번째마다: fma 로 잡힘, chain 으로 안 잡힘");
}

#[test]
fn m4_few_cores_only() {
    let model = FaultModel::FewCoresOnly(Flip { lane: 3, bit: 17 });
    // 전 코어 동시 부하에서는 "한두 코어만 도는" 조건이 오지 않는다
    assert!(!run(KernelSet::Chain, Pattern::Steady, 4, model), "M4 가 4스레드 동시에서 잡혔다");
    // 코어 순환: 한 번에 한 코어만 돌아 조건이 온다
    assert!(run(KernelSet::Chain, Pattern::Cycle, 4, model), "M4 를 코어 순환으로 못 잡음");
    eprintln!("M4 한두 코어일 때만: 4스레드 동시 안 잡힘, 코어 순환 잡힘");
}

#[test]
fn m5_after_wake_only() {
    let model = FaultModel::AfterWake(Flip { lane: 2, bit: 9 });
    // 쉬지 않는 steady 에서는 깨어나는 순간이 없다
    assert!(caught_at(KernelSet::Chain, Pattern::Steady, 2, model).is_none(), "M5 가 steady 에서 잡혔다");
    // pulse·cycle 은 쉬었다 깨어나므로 잡힌다 — 오류는 코어 0 의 깨어난 첫 블록
    for pattern in [Pattern::Pulse, Pattern::Cycle] {
        let e = caught_at(KernelSet::Chain, pattern, 4, model).unwrap_or_else(|| panic!("M5 를 {pattern:?} 로 못 잡음"));
        assert_eq!(e.cpu, 0);
        assert!(e.block > 0, "깨어나기 전 블록에서 잡혔다: {e:?}");
    }
    eprintln!("M5 깨어난 직후만: steady 안 잡힘, pulse·코어 순환 잡힘");
}

#[test]
fn start_flip_matches_old_injection() {
    assert!(run(KernelSet::Wide, Pattern::Steady, 2, FaultModel::StartFlip(Flip { lane: 9, bit: 40 })));
}
