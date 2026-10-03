//! 실제 불량 사례를 흉내 낸 모델로 검출 능력을 잰다.
//! 빈틈 모델은 지금 "안 잡힘"을 단언한다 — 빈틈을 메우는 작업에서 "잡힘"으로 바꾼다.

use std::time::Duration;
use steadycheck::cpu::{self, CpuConfig, KernelSet, Pattern};
use steadycheck::fault::{FaultInject, FaultModel};
use steadycheck::kernel::{Flip, Isa};
use steadycheck::mem::{self, MemConfig, MemFault};

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

// 기존 run(…) 은 블록 2 에 주입한다. mix 에서 lz 차례(커널 5개 중 5번째 → 블록 4)에 넣으려고 블록을 고르는 헬퍼를 둔다.
fn caught_at_block(kernels: KernelSet, threads: usize, model: FaultModel, block: u64) -> bool {
    let out = cpu::run(&CpuConfig {
        isa: Isa::best(), threads, duration: Duration::from_secs(10), kernels, pattern: Pattern::Steady,
        iters: Some(1 << 12), inject: None, rotate_isa: false, fault: Some(FaultInject { cpu: 0, block, model }),
    });
    if let Some(e) = &out.error {
        assert_eq!((e.cpu, e.block), (0, block), "엉뚱한 곳에서 검출: {e:?}");
    }
    out.error.is_some()
}

#[test]
fn m6_byte_neighbor_store() {
    let model = FaultModel::ByteNeighborStore { every: 1_000 };
    assert!(caught_at_block(KernelSet::Lz, 2, model, 2), "M6 를 lz 로 못 잡음");
    assert!(caught_at_block(KernelSet::Mix, 2, model, 4), "M6 를 mix 의 lz 차례에서 못 잡음");
    assert!(!caught_at_block(KernelSet::Chain, 2, model, 2), "chain 이 바이트 저장 불량을 잡았다 — 흉내가 잘못됨");
    eprintln!("M6 바이트 이웃 저장: lz·mix 잡힘, chain 안 잡힘");
}

#[test]
fn start_flip_matches_old_injection() {
    assert!(run(KernelSet::Wide, Pattern::Steady, 2, FaultModel::StartFlip(Flip { lane: 9, bit: 40 })));
}

fn mem_error(fault: MemFault, threads: usize) -> Option<mem::MemError> {
    mem::run(&MemConfig { mb: 8, duration: Duration::from_secs(2), threads, inject: None, fault: Some(fault) }).error
}

#[test]
fn m9_mem_busy_only() {
    let f = MemFault::BusyOnly { word: 4321, bit: 5, min_active: 4 };
    assert!(mem_error(f, 1).is_none(), "M9 가 일꾼 하나로 잡혔다 — 흉내가 잘못됨");
    // 일꾼 넷이 동시에 두드리면 조건이 온다
    let e = mem_error(f, 4).expect("M9 를 일꾼 넷으로 못 잡음");
    assert_eq!((e.thread, e.offset_bytes), (0, 4321 * 8));
    // 전송 중 오류: 비트 5 만 틀리고, 다시 읽으면 정상 값
    assert_eq!(e.actual, format!("{:#018x}", 0x5555_5555_5555_5555u64 ^ (1 << 5)));
    assert_eq!(e.reread, e.expected);
    eprintln!("M9 메모리 바쁠 때만: 일꾼 하나 안 잡힘, 일꾼 넷 잡힘");
}

#[test]
fn m11_coupling_up() {
    let f = MemFault::CouplingUp { word: 1000, distance: 64, bit: 3 };
    // 2단계에서 가해 칸에 뒤집은 값을 쓸 때 번지고, 피해 칸은 아직 안 읽었으므로 거기서 잡힌다
    let e = mem_error(f, 4).expect("M11 을 3단계 패스로 못 잡음");
    assert_eq!((e.thread, e.pass, e.offset_bytes), (0, 0, 1064 * 8));
    assert_eq!(e.actual, e.reread, "번진 값은 메모리에 그대로 남는다");
    eprintln!("M11 이웃 칸 간섭: 3단계 패스 잡힘");
}
