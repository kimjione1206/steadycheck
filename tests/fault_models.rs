//! 실제 불량 사례를 흉내 낸 모델로 검출 능력을 잰다.
//! 빈틈 모델은 지금 "안 잡힘"을 단언한다 — 빈틈을 메우는 작업에서 "잡힘"으로 바꾼다.

use std::time::Duration;
use steadycheck::cpu::{self, CpuConfig, KernelSet, Pattern};
use steadycheck::fault::{FaultInject, FaultModel};
use steadycheck::kernel::{Flip, Isa};
use steadycheck::mem::{self, MemConfig, MemFault};
use steadycheck::share::{self, ShareConfig, ShareInject};

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
        // 느린 기계에서는 정답표를 만드는 동안 차례가 지나 일꾼이 첫 블록 전에 쉬었다 깨어날 수 있어 block 0 에서도 정상 발동한다 — 깨어난 첫 블록만 고르는 규칙은 fault_for_block 단위 시험이 고정한다
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
    // 전송 중 오류: 첫 읽기(A 주소고유값)에서 비트 5 만 틀리고, 다시 읽으면 정상 값
    assert_eq!((e.stage, e.element), ("A", 1));
    let want = u64::from_str_radix(e.expected.trim_start_matches("0x"), 16).unwrap();
    assert_eq!(e.actual, format!("{:#018x}", want ^ (1 << 5)));
    assert_eq!(e.reread, e.expected);
    // 칸은 멀쩡하고 읽는 길에서만 틀림 → read, 칸 4321 = 줄 안 칸 1 → 위치 64 + 5
    assert_eq!((e.kind, e.line_bits.clone()), ("read", vec![69]));
    eprintln!("M9 메모리 바쁠 때만: 일꾼 하나 안 잡힘, 일꾼 넷 잡힘");
}

#[test]
fn m11_coupling_up() {
    let f = MemFault::CouplingUp { word: 1000, distance: 64, bit: 3 };
    // A 의 쓰기에서 번진 값은 피해 칸을 나중에 쓰며 덮인다. March C- ⇑(r0,w1) 에서 가해 칸에 1 을 쓸 때 번지고,
    // 피해 칸은 아직 안 읽었으므로 거기서 잡힌다
    let e = mem_error(f, 4).expect("M11 을 기본 세트로 못 잡음");
    assert_eq!((e.thread, e.pass, e.stage, e.element, e.offset_bytes), (0, 1, "B", 1, 1064 * 8));
    assert_eq!(e.actual, e.reread, "번진 값은 메모리에 그대로 남는다");
    // 칸 1064 = 줄 안 칸 0 → 위치 3
    assert_eq!((e.kind, e.line_bits.clone()), ("stored", vec![3]));
    // 뒤쪽 조각(일꾼 4 → 조각 2 는 칸 524,288 부터)에 있어도 조각 안 번호로 옮겨 걸려 그 일꾼이 잡는다
    let f = MemFault::CouplingUp { word: 524_288 + 1000, distance: 64, bit: 3 };
    let e = mem_error(f, 4).expect("M11 을 뒤쪽 조각에서 못 잡음");
    assert_eq!((e.thread, e.stage, e.element, e.offset_bytes), (2, "B", 1, (524_288 + 1064) * 8));
    eprintln!("M11 이웃 칸 간섭: 기본 세트 잡힘");
}

#[test]
fn m7_stale_read() {
    // 받는 쪽이 직전 순번의 옛 값을 읽으면 대조에서 걸린다
    let out = share::run(&ShareConfig { threads: 4, duration: Duration::from_secs(5), inject: Some(ShareInject { cpu: 2, msg: 4 }) });
    let e = out.error.expect("M7 을 share 로 못 잡음");
    assert_eq!((e.cpu, e.from, e.seq, e.word), (2, 1, 4, 0));
    eprintln!("M7 옛 값 읽기: share 잡힘");
}
