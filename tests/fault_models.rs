//! 실제 불량 사례를 흉내 낸 모델로 검출 능력을 잰다.
//! 빈틈 모델은 지금 "안 잡힘"을 단언한다 — 빈틈을 메우는 작업에서 "잡힘"으로 바꾼다.

use std::time::Duration;
use steadycheck::cpu::{self, CpuConfig, KernelSet, Pattern};
use steadycheck::fault::{FaultInject, FaultModel};
use steadycheck::kernel::{Flip, Isa};

fn run(kernels: KernelSet, threads: usize, model: FaultModel) -> bool {
    let out = cpu::run(&CpuConfig {
        isa: Isa::best(), threads, duration: Duration::from_secs(10), kernels, pattern: Pattern::Steady,
        iters: Some(1 << 12), inject: None, fault: Some(FaultInject { cpu: 0, block: 2, model }),
    });
    if let Some(e) = &out.error {
        assert_eq!((e.cpu, e.block), (0, 2), "엉뚱한 곳에서 검출: {e:?}");
    }
    out.error.is_some()
}

#[test]
fn m1_fma_exponent_conditional() {
    let caught = run(KernelSet::Fma, 2, FaultModel::FmaExpConditional { bit: 20 });
    // 빈틈: 지금 fma 입력은 모두 [1,2) 라 지수 조건 불량이 켜지지 않는다
    assert!(!caught, "M1 이 잡혔다 — 입력 넓힘 작업의 단언을 확인할 것");
    eprintln!("M1 FMA 입력 지수 조건: 안 잡힘(빈틈)");
}

#[test]
fn m2_fma_lane_every_nth() {
    let model = FaultModel::FmaLaneEveryNth { lane: 7, n: 1000, bit: 20 };
    assert!(run(KernelSet::Fma, 2, model), "M2 를 fma 로 못 잡음");
    // chain 은 FMA 장치를 쓰지 않으므로 이 불량을 지나지 않는다
    assert!(!run(KernelSet::Chain, 2, model), "chain 이 FMA 불량을 잡았다 — 흉내가 잘못됨");
    eprintln!("M2 FMA 줄 하나 N번째마다: fma 로 잡힘, chain 으로 안 잡힘");
}

#[test]
fn m4_few_cores_only() {
    let model = FaultModel::FewCoresOnly(Flip { lane: 3, bit: 17 });
    // 빈틈: 전 코어 동시 부하에서는 "한두 코어만 도는" 조건이 오지 않는다
    assert!(!run(KernelSet::Chain, 4, model), "M4 가 4스레드에서 잡혔다");
    assert!(run(KernelSet::Chain, 2, model), "M4 를 2스레드에서 못 잡음");
    eprintln!("M4 스레드 2개 이하일 때만: 4스레드 안 잡힘(빈틈), 2스레드 잡힘");
}

#[test]
fn start_flip_matches_old_injection() {
    assert!(run(KernelSet::Wide, 2, FaultModel::StartFlip(Flip { lane: 9, bit: 40 })));
}
