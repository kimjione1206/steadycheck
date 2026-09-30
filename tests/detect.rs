//! 채점표: 일부러 넣은 오류를 전부, 제자리에서, 1초 안에 잡는가. 주입 없으면 오류 0 인가.

use std::time::Duration;
use steadycheck::cpu::{self, CpuConfig, CpuInject};
use steadycheck::kernel::{Flip, Isa};
use steadycheck::mem::{self, MemConfig, MemInject};

const ROUNDS: usize = 200;

fn supported_isas() -> Vec<Isa> {
    [Isa::Scalar, Isa::Avx2, Isa::Avx512].into_iter().filter(|i| i.supported()).collect()
}

#[test]
fn cpu_catches_every_injected_flip() {
    for isa in supported_isas() {
        let mut caught = 0;
        for k in 0..ROUNDS {
            let inj = CpuInject {
                cpu: k % 2,
                block: (k % 5) as u64,
                flip: Flip { lane: k % 8, bit: ((k * 7) % 64) as u32 },
            };
            let out = cpu::run(&CpuConfig { isa, threads: 2, duration: Duration::from_secs(10), kernels: cpu::KernelSet::Chain, pattern: cpu::Pattern::Steady, iters: Some(1 << 12), inject: Some(inj) });
            let e = out.error.unwrap_or_else(|| panic!("{isa:?} 주입 {k} 놓침"));
            assert_eq!((e.cpu, e.block), (inj.cpu, inj.block), "{isa:?} 주입 {k} 위치 틀림");
            assert!(e.at_ms - e.block_start_ms < 1000, "{isa:?} 주입 {k} 검출 지연 {}ms", e.at_ms - e.block_start_ms);
            caught += 1;
        }
        eprintln!("{isa:?}: {caught}/{ROUNDS} 검출");
    }
}

#[test]
fn mem_catches_every_injected_flip() {
    let mb = 4;
    let words = mb * 1024 * 1024 / 8;
    for k in 0..ROUNDS {
        let inj = MemInject { pass: (k % 5) as u64, word: (k * 7919) % words, bit: (k % 64) as u32 };
        let out = mem::run(&MemConfig { mb, duration: Duration::from_secs(10), inject: Some(inj) });
        let e = out.error.unwrap_or_else(|| panic!("메모리 주입 {k} 놓침"));
        assert_eq!((e.pass, e.offset_bytes), (inj.pass, inj.word * 8), "메모리 주입 {k} 위치 틀림");
        assert!(e.at_ms - e.pass_start_ms < 1000, "메모리 주입 {k} 검출 지연 {}ms", e.at_ms - e.pass_start_ms);
    }
    eprintln!("mem: {ROUNDS}/{ROUNDS} 검출");
}

#[test]
fn no_false_positive_without_injection() {
    for isa in supported_isas() {
        let out = cpu::run(&CpuConfig { isa, threads: 4, duration: Duration::from_secs(3), kernels: cpu::KernelSet::Chain, pattern: cpu::Pattern::Steady, iters: Some(1 << 14), inject: None });
        assert!(!out.failed(), "{isa:?} 오탐: {:?}", out.error);
    }
    let out = mem::run(&MemConfig { mb: 64, duration: Duration::from_secs(3), inject: None });
    assert!(!out.failed(), "메모리 오탐: {:?}", out.error);
}
