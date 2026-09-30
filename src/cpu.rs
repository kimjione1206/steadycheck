//! CPU 검사: 정답표를 확정하고, 논리 CPU 마다 고정된 스레드가 블록을 계산해 대조한다.

use crate::kernel::{run_block, Flip, Isa};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const GOLDEN_SEEDS: u64 = 16;

#[derive(Clone, Copy, Debug)]
pub struct CpuInject {
    pub cpu: usize,
    pub block: u64,
    pub flip: Flip,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub struct CpuError {
    pub cpu: usize,
    pub block: u64,
    pub seed: u64,
    pub expected: String,
    pub actual: String,
    pub block_start_ms: u64,
    pub at_ms: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct CpuOutcome {
    pub isa: Isa,
    pub threads: usize,
    pub pinned: bool,
    pub blocks: u64,
    pub elapsed_ms: u64,
    pub golden_unstable: bool,
    pub error: Option<CpuError>,
}

impl CpuOutcome {
    pub fn failed(&self) -> bool {
        self.golden_unstable || self.error.is_some()
    }
}

pub struct CpuConfig {
    pub isa: Isa,
    pub threads: usize,
    pub duration: Duration,
    pub iters: u64,
    pub inject: Option<CpuInject>,
}

/// 정답표는 스칼라로 두 번 계산해 같을 때만 쓴다. 다르면 이 CPU 는 이미 틀린 것.
pub fn goldens(iters: u64) -> Option<Vec<u64>> {
    let once = || (0..GOLDEN_SEEDS).map(|s| run_block(Isa::Scalar, s, iters, None)).collect::<Vec<_>>();
    let (a, b) = (once(), once());
    (a == b).then_some(a)
}

pub fn run(cfg: &CpuConfig) -> CpuOutcome {
    let start = Instant::now();
    // move: 워커 스레드에서도 쓰려면 start 를 값으로 들고 있어야 한다
    let ms = move |t: Instant| t.duration_since(start).as_millis() as u64;
    let Some(gold) = goldens(cfg.iters) else {
        return CpuOutcome {
            isa: cfg.isa, threads: cfg.threads, pinned: false, blocks: 0,
            elapsed_ms: ms(Instant::now()), golden_unstable: true, error: None,
        };
    };
    let gold = Arc::new(gold);
    let stop = Arc::new(AtomicBool::new(false));
    let first_error: Arc<Mutex<Option<CpuError>>> = Arc::new(Mutex::new(None));
    let deadline = Instant::now() + cfg.duration;

    let handles: Vec<_> = (0..cfg.threads)
        .map(|cpu| {
            let (gold, stop, first_error) = (gold.clone(), stop.clone(), first_error.clone());
            let (isa, iters, inject) = (cfg.isa, cfg.iters, cfg.inject);
            std::thread::spawn(move || {
                let pinned = crate::affinity::pin_current_thread(cpu);
                let mut block = 0u64;
                while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                    // 코어마다 시드를 어긋나게 해 같은 순간 서로 다른 문제를 푼다
                    let seed = (block + cpu as u64) % GOLDEN_SEEDS;
                    let flip = inject.filter(|i| i.cpu == cpu && i.block == block).map(|i| i.flip);
                    let block_start = Instant::now();
                    let got = run_block(isa, seed, iters, flip);
                    let want = gold[seed as usize];
                    if got != want {
                        let mut slot = first_error.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(CpuError {
                                cpu, block, seed,
                                expected: format!("{want:#018x}"),
                                actual: format!("{got:#018x}"),
                                block_start_ms: ms(block_start),
                                at_ms: ms(Instant::now()),
                            });
                        }
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    block += 1;
                }
                (pinned, block)
            })
        })
        .collect();

    let mut blocks = 0;
    let mut pinned = true;
    for h in handles {
        let (p, b) = h.join().expect("워커 스레드가 죽었다");
        pinned &= p;
        blocks += b;
    }
    let error = first_error.lock().unwrap().clone();
    CpuOutcome { isa: cfg.isa, threads: cfg.threads, pinned, blocks, elapsed_ms: ms(Instant::now()), golden_unstable: false, error }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{Flip, Isa};
    use std::time::Duration;

    fn cfg(inject: Option<CpuInject>) -> CpuConfig {
        CpuConfig { isa: Isa::best(), threads: 2, duration: Duration::from_secs(2), iters: 1 << 12, inject }
    }

    #[test]
    fn goldens_are_stable() {
        let g = goldens(1 << 12).expect("정답표가 두 번 같아야 한다");
        assert_eq!(g.len(), GOLDEN_SEEDS as usize);
    }

    #[test]
    fn clean_run_has_no_error() {
        let out = run(&cfg(None));
        assert!(out.error.is_none(), "{:?}", out.error);
        assert!(!out.failed());
        assert!(out.blocks > 0);
    }

    #[test]
    fn injected_flip_is_caught_at_exact_place() {
        let inj = CpuInject { cpu: 1, block: 3, flip: Flip { lane: 5, bit: 40 } };
        let out = run(&cfg(Some(inj)));
        let e = out.error.clone().expect("주입한 오류를 잡아야 한다");
        assert_eq!((e.cpu, e.block), (1, 3));
        assert_ne!(e.expected, e.actual);
        assert!(out.failed());
    }

    // 변이 테스트 보강: 시드 배정·블록 합계·고정 여부가 정확해야 한다
    #[test]
    fn error_report_numbers_are_exact() {
        let inj = CpuInject { cpu: 1, block: 3, flip: Flip { lane: 5, bit: 40 } };
        let e = run(&cfg(Some(inj))).error.expect("주입한 오류를 잡아야 한다");
        assert_eq!(e.seed, (3 + 1) % GOLDEN_SEEDS);

        // 스레드 하나면 블록 3 에서 멈추므로 합계는 정확히 3
        let inj = CpuInject { cpu: 0, block: 3, flip: Flip { lane: 5, bit: 40 } };
        let out = run(&CpuConfig { isa: Isa::best(), threads: 1, duration: Duration::from_secs(2), iters: 1 << 12, inject: Some(inj) });
        assert_eq!(out.blocks, 3);
        let can_pin = std::thread::spawn(|| crate::affinity::pin_current_thread(0)).join().unwrap();
        assert_eq!(out.pinned, can_pin);
    }
}
