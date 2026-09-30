//! CPU 검사: 커널별 정답표를 확정하고, 논리 CPU 마다 고정된 스레드가 블록을 계산해 대조한다.

use crate::kernel::{run_block, Flip, Isa, Kernel};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const GOLDEN_SEEDS: u64 = 16;
/// 정답표 자체 점검: 이 길이에서는 스칼라와 선택 명령어 세트가 같아야 한다
pub const SELF_CHECK_ITERS: u64 = 4096;
/// pulse 패턴: 이만큼 켜고 이만큼 끈다
pub const PULSE_MS: u64 = 250;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KernelSet {
    Chain,
    Wide,
    Fma,
    Mix,
}

impl KernelSet {
    pub fn parse(s: &str) -> Option<KernelSet> {
        match s {
            "chain" => Some(KernelSet::Chain),
            "wide" => Some(KernelSet::Wide),
            "fma" => Some(KernelSet::Fma),
            "mix" => Some(KernelSet::Mix),
            _ => None,
        }
    }

    pub fn kernels(self) -> Vec<Kernel> {
        match self {
            KernelSet::Chain => vec![Kernel::Chain],
            KernelSet::Wide => vec![Kernel::Wide],
            KernelSet::Fma => vec![Kernel::Fma],
            KernelSet::Mix => Kernel::ALL.to_vec(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Pattern {
    Steady,
    Pulse,
}

impl Pattern {
    pub fn parse(s: &str) -> Option<Pattern> {
        match s {
            "steady" => Some(Pattern::Steady),
            "pulse" => Some(Pattern::Pulse),
            _ => None,
        }
    }
}

/// pulse 의 꺼진 구간이면 다음 켜짐까지 기다릴 ms, 켜진 구간이면 None.
/// 모든 워커가 같은 시작 시각을 기준으로 삼아 부하가 한꺼번에 켜지고 꺼진다.
pub fn pulse_wait(elapsed_ms: u64) -> Option<u64> {
    ((elapsed_ms / PULSE_MS) % 2 == 1).then(|| PULSE_MS - elapsed_ms % PULSE_MS)
}

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
    pub kernel: Kernel,
    pub seed: u64,
    pub expected: String,
    pub actual: String,
    pub block_start_ms: u64,
    pub at_ms: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct CpuOutcome {
    pub isa: Isa,
    pub kernels: KernelSet,
    pub pattern: Pattern,
    pub threads: usize,
    pub pinned: bool,
    pub blocks: u64,
    /// 줄 × 반복 합계 — 커널이 달라도 비교할 수 있는 계산량
    pub lane_iters: u64,
    /// 정답표를 만든 뒤 워커가 돈 시간
    pub run_ms: u64,
    pub lane_iters_per_sec: u64,
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
    pub kernels: KernelSet,
    pub pattern: Pattern,
    /// None 이면 커널별 기본 반복 수
    pub iters: Option<u64>,
    pub inject: Option<CpuInject>,
}

/// 정답표는 두 조건을 모두 만족할 때만 쓴다.
/// 1) 짧은 길이에서 스칼라(명세)와 선택 명령어 세트가 같다
/// 2) 실제 길이를 선택 명령어 세트로 두 번 계산해 같다 (스칼라 FMA 는 x86 에서 너무 느려서)
pub fn goldens(kernel: Kernel, isa: Isa, iters: u64) -> Option<Vec<u64>> {
    let agree = (0..GOLDEN_SEEDS).all(|s| {
        run_block(kernel, Isa::Scalar, s, SELF_CHECK_ITERS, None) == run_block(kernel, isa, s, SELF_CHECK_ITERS, None)
    });
    let once = || (0..GOLDEN_SEEDS).map(|s| run_block(kernel, isa, s, iters, None)).collect::<Vec<_>>();
    let (a, b) = (once(), once());
    (agree && a == b).then_some(a)
}

struct Table {
    kernel: Kernel,
    iters: u64,
    gold: Vec<u64>,
}

pub fn run(cfg: &CpuConfig) -> CpuOutcome {
    let start = Instant::now();
    // move: 워커 스레드에서도 쓰려면 start 를 값으로 들고 있어야 한다
    let ms = move |t: Instant| t.duration_since(start).as_millis() as u64;
    let mut tables = Vec::new();
    for kernel in cfg.kernels.kernels() {
        let iters = cfg.iters.unwrap_or(kernel.default_iters());
        match goldens(kernel, cfg.isa, iters) {
            Some(gold) => tables.push(Table { kernel, iters, gold }),
            None => {
                return CpuOutcome {
                    isa: cfg.isa, kernels: cfg.kernels, pattern: cfg.pattern, threads: cfg.threads, pinned: false,
                    blocks: 0, lane_iters: 0, run_ms: 0, lane_iters_per_sec: 0,
                    elapsed_ms: ms(Instant::now()), golden_unstable: true, error: None,
                };
            }
        }
    }
    let tables = Arc::new(tables);
    let stop = Arc::new(AtomicBool::new(false));
    let first_error: Arc<Mutex<Option<CpuError>>> = Arc::new(Mutex::new(None));
    let run_start = Instant::now();
    let deadline = run_start + cfg.duration;

    let handles: Vec<_> = (0..cfg.threads)
        .map(|cpu| {
            let (tables, stop, first_error) = (tables.clone(), stop.clone(), first_error.clone());
            let (isa, pattern, inject) = (cfg.isa, cfg.pattern, cfg.inject);
            std::thread::spawn(move || {
                let pinned = crate::affinity::pin_current_thread(cpu);
                let mut block = 0u64;
                let mut lane_iters = 0u64;
                while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                    if pattern == Pattern::Pulse {
                        if let Some(wait) = pulse_wait(run_start.elapsed().as_millis() as u64) {
                            std::thread::sleep(Duration::from_millis(wait));
                            continue;
                        }
                    }
                    // 블록마다 커널을 돌아가며 쓰고, 코어마다 시드를 어긋나게 한다
                    let t = &tables[(block % tables.len() as u64) as usize];
                    let seed = (block + cpu as u64) % GOLDEN_SEEDS;
                    let flip = inject.filter(|i| i.cpu == cpu && i.block == block).map(|i| i.flip);
                    let block_start = Instant::now();
                    let got = run_block(t.kernel, isa, seed, t.iters, flip);
                    let want = t.gold[seed as usize];
                    if got != want {
                        let mut slot = first_error.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(CpuError {
                                cpu, block, kernel: t.kernel, seed,
                                expected: format!("{want:#018x}"),
                                actual: format!("{got:#018x}"),
                                block_start_ms: ms(block_start),
                                at_ms: ms(Instant::now()),
                            });
                        }
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    lane_iters += t.iters * t.kernel.lanes();
                    block += 1;
                }
                (pinned, block, lane_iters)
            })
        })
        .collect();

    let mut blocks = 0;
    let mut lane_iters = 0;
    let mut pinned = true;
    for h in handles {
        let (p, b, l) = h.join().expect("워커 스레드가 죽었다");
        pinned &= p;
        blocks += b;
        lane_iters += l;
    }
    let run_ms = run_start.elapsed().as_millis() as u64;
    let lane_iters_per_sec = if run_ms == 0 { 0 } else { lane_iters * 1000 / run_ms };
    let error = first_error.lock().unwrap().clone();
    CpuOutcome {
        isa: cfg.isa, kernels: cfg.kernels, pattern: cfg.pattern, threads: cfg.threads, pinned, blocks,
        lane_iters, run_ms, lane_iters_per_sec, elapsed_ms: ms(Instant::now()), golden_unstable: false, error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{Flip, Isa};
    use std::time::Duration;

    fn cfg(kernels: KernelSet, inject: Option<CpuInject>) -> CpuConfig {
        CpuConfig {
            isa: Isa::best(), threads: 2, duration: Duration::from_secs(2), kernels,
            pattern: Pattern::Steady, iters: Some(1 << 12), inject,
        }
    }

    #[test]
    fn goldens_are_stable() {
        for k in Kernel::ALL {
            let g = goldens(k, Isa::best(), 1 << 12).expect("정답표가 두 번 같아야 한다");
            assert_eq!(g.len(), GOLDEN_SEEDS as usize);
            // 정답표는 명세(스칼라)와도 같아야 한다
            assert_eq!(g[5], run_block(k, Isa::Scalar, 5, 1 << 12, None), "{k:?}");
        }
    }

    #[test]
    fn clean_run_has_no_error() {
        for set in [KernelSet::Chain, KernelSet::Wide, KernelSet::Fma, KernelSet::Mix] {
            let out = run(&cfg(set, None));
            assert!(out.error.is_none(), "{set:?} {:?}", out.error);
            assert!(!out.failed());
            assert!(out.blocks > 0);
            assert!(out.lane_iters > 0 && out.lane_iters_per_sec > 0);
        }
    }

    #[test]
    fn injected_flip_is_caught_at_exact_place() {
        let inj = CpuInject { cpu: 1, block: 3, flip: Flip { lane: 5, bit: 40 } };
        let out = run(&cfg(KernelSet::Chain, Some(inj)));
        let e = out.error.clone().expect("주입한 오류를 잡아야 한다");
        assert_eq!((e.cpu, e.block, e.kernel), (1, 3, Kernel::Chain));
        assert_ne!(e.expected, e.actual);
        assert!(out.failed());
    }

    // 변이 테스트 보강: 시드 배정·블록 합계·고정 여부가 정확해야 한다
    #[test]
    fn error_report_numbers_are_exact() {
        let inj = CpuInject { cpu: 1, block: 3, flip: Flip { lane: 5, bit: 40 } };
        let e = run(&cfg(KernelSet::Chain, Some(inj))).error.expect("주입한 오류를 잡아야 한다");
        assert_eq!(e.seed, (3 + 1) % GOLDEN_SEEDS);

        // 스레드 하나면 블록 3 에서 멈추므로 합계는 정확히 3
        let inj = CpuInject { cpu: 0, block: 3, flip: Flip { lane: 5, bit: 40 } };
        let out = run(&CpuConfig { threads: 1, ..cfg(KernelSet::Chain, Some(inj)) });
        assert_eq!(out.blocks, 3);
        assert_eq!(out.lane_iters, 3 * (1 << 12) * 8);
        let can_pin = std::thread::spawn(|| crate::affinity::pin_current_thread(0)).join().unwrap();
        assert_eq!(out.pinned, can_pin);
    }

    // mix 는 블록 번호 순서대로 chain → wide → fma 를 쓴다
    #[test]
    fn mix_rotates_kernels_by_block() {
        for (block, want) in [(0, Kernel::Chain), (1, Kernel::Wide), (2, Kernel::Fma), (3, Kernel::Chain), (5, Kernel::Fma)] {
            let inj = CpuInject { cpu: 0, block, flip: Flip { lane: 1, bit: 3 } };
            let e = run(&CpuConfig { threads: 1, ..cfg(KernelSet::Mix, Some(inj)) }).error.expect("잡아야 한다");
            assert_eq!((e.block, e.kernel), (block, want));
        }
    }

    #[test]
    fn pulse_wait_windows() {
        assert_eq!(pulse_wait(0), None);
        assert_eq!(pulse_wait(PULSE_MS - 1), None);
        assert_eq!(pulse_wait(PULSE_MS), Some(PULSE_MS));
        assert_eq!(pulse_wait(2 * PULSE_MS - 1), Some(1));
        assert_eq!(pulse_wait(2 * PULSE_MS), None);
    }

    #[test]
    fn pulse_run_is_clean() {
        let out = run(&CpuConfig { pattern: Pattern::Pulse, ..cfg(KernelSet::Mix, None) });
        assert!(!out.failed() && out.blocks > 0, "{:?}", out.error);
    }

    #[test]
    fn kernel_set_parse() {
        assert_eq!(KernelSet::parse("mix"), Some(KernelSet::Mix));
        assert_eq!(KernelSet::parse("fma"), Some(KernelSet::Fma));
        assert_eq!(KernelSet::parse("avx"), None);
        assert_eq!(KernelSet::Mix.kernels(), Kernel::ALL.to_vec());
        assert_eq!(Pattern::parse("pulse"), Some(Pattern::Pulse));
        assert_eq!(Pattern::parse("burst"), None);
    }
}
