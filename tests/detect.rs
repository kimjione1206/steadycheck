//! 채점표: 일부러 넣은 오류를 전부, 제자리에서, 1초 안에 잡는가. 주입 없으면 오류 0 인가.

use std::time::Duration;
use steadycheck::cpu::{self, CpuConfig, CpuInject, KernelSet, Pattern};
use steadycheck::kernel::{Flip, Isa, Kernel};
use steadycheck::mem::{self, MemConfig, MemInject};

const ROUNDS: usize = 200;

// DETECT_ISA 가 있으면 그 명령어 세트만 (흉내 CPU 에서 시간 절약)
fn supported_isas() -> Vec<Isa> {
    let only = std::env::var("DETECT_ISA").ok().and_then(|s| Isa::parse(&s));
    [Isa::Scalar, Isa::Avx2, Isa::Avx512]
        .into_iter()
        .filter(|i| i.supported() && only.is_none_or(|o| o == *i))
        .collect()
}

fn single(k: Kernel) -> KernelSet {
    match k {
        Kernel::Chain => KernelSet::Chain,
        Kernel::Wide => KernelSet::Wide,
        Kernel::Fma => KernelSet::Fma,
        Kernel::Fma32 => KernelSet::Fma32,
    }
}

#[test]
fn cpu_catches_every_injected_flip() {
    for kernel in Kernel::ALL {
        for isa in supported_isas() {
            let mut caught = 0;
            for k in 0..ROUNDS {
                // lane 0~63: fma·fma32 는 32 이상이면 누적 실수 쪽, 나머지 커널은 자기 줄 수로 나눈 나머지
                let inj = CpuInject { cpu: k % 2, block: (k % 5) as u64, flip: Flip { lane: k % 64, bit: ((k * 7) % 64) as u32 } };
                let out = cpu::run(&CpuConfig {
                    isa, threads: 2, duration: Duration::from_secs(10), kernels: single(kernel),
                    pattern: Pattern::Steady, iters: Some(1 << 12), inject: Some(inj), rotate_isa: false, fault: None,
                });
                let e = out.error.unwrap_or_else(|| panic!("{kernel:?}/{isa:?} 주입 {k} 놓침"));
                assert_eq!((e.cpu, e.block, e.kernel), (inj.cpu, inj.block, kernel), "{kernel:?}/{isa:?} 주입 {k} 위치 틀림");
                assert!(e.at_ms - e.block_start_ms < 1000, "{kernel:?}/{isa:?} 주입 {k} 검출 지연 {}ms", e.at_ms - e.block_start_ms);
                caught += 1;
            }
            eprintln!("{kernel:?}/{isa:?}: {caught}/{ROUNDS} 검출");
        }
    }
}

#[test]
fn mem_catches_every_injected_flip() {
    let (mb, threads) = (4, 4);
    let words = mb * 1024 * 1024 / 8;
    let per = words / threads;
    for k in 0..ROUNDS {
        let inj = MemInject { pass: (k % 5) as u64, word: (k * 7919) % words, bit: (k % 64) as u32 };
        let out = mem::run(&MemConfig { mb, duration: Duration::from_secs(10), threads, inject: Some(inj), fault: None });
        let e = out.error.unwrap_or_else(|| panic!("메모리 주입 {k} 놓침"));
        assert_eq!((e.thread, e.pass, e.offset_bytes), ((inj.word / per).min(threads - 1), inj.pass, inj.word * 8), "메모리 주입 {k} 위치 틀림");
        assert!(e.at_ms - e.pass_start_ms < 1000, "메모리 주입 {k} 검출 지연 {}ms", e.at_ms - e.pass_start_ms);
    }
    eprintln!("mem: {ROUNDS}/{ROUNDS} 검출 (일꾼 {threads})");
}

#[test]
fn no_false_positive_without_injection() {
    for isa in supported_isas() {
        for (kernels, pattern) in [
            (KernelSet::Chain, Pattern::Steady), (KernelSet::Wide, Pattern::Steady),
            (KernelSet::Fma, Pattern::Steady), (KernelSet::Fma32, Pattern::Steady), (KernelSet::Mix, Pattern::Pulse),
            (KernelSet::Mix, Pattern::Cycle),
        ] {
            let out = cpu::run(&CpuConfig {
                isa, threads: 4, duration: Duration::from_secs(3), kernels, pattern,
                iters: Some(1 << 14), inject: None, rotate_isa: false, fault: None,
            });
            assert!(!out.failed(), "{kernels:?}/{pattern:?}/{isa:?} 오탐: {:?}", out.error);
        }
    }
    let out = mem::run(&MemConfig { mb: 64, duration: Duration::from_secs(3), threads: 4, inject: None, fault: None });
    assert!(!out.failed(), "메모리 오탐: {:?}", out.error);
}
