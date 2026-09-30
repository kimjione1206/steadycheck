//! CPU 검사: 커널별 정답표를 확정하고, 논리 CPU 마다 고정된 스레드가 블록을 계산해 대조한다.

use crate::fault::FaultInject;
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
    Fma32,
    Mix,
}

impl KernelSet {
    pub fn parse(s: &str) -> Option<KernelSet> {
        match s {
            "chain" => Some(KernelSet::Chain),
            "wide" => Some(KernelSet::Wide),
            "fma" => Some(KernelSet::Fma),
            "fma32" => Some(KernelSet::Fma32),
            "mix" => Some(KernelSet::Mix),
            _ => None,
        }
    }

    pub fn kernels(self) -> Vec<Kernel> {
        match self {
            KernelSet::Chain => vec![Kernel::Chain],
            KernelSet::Wide => vec![Kernel::Wide],
            KernelSet::Fma => vec![Kernel::Fma],
            KernelSet::Fma32 => vec![Kernel::Fma32],
            KernelSet::Mix => Kernel::ALL.to_vec(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Pattern {
    Steady,
    Pulse,
    /// 한 번에 코어 하나만 돌고 나머지는 쉰다 — 한두 코어가 최고 클럭일 때만 틀리는 CPU 용
    Cycle,
}

impl Pattern {
    pub fn parse(s: &str) -> Option<Pattern> {
        match s {
            "steady" => Some(Pattern::Steady),
            "pulse" => Some(Pattern::Pulse),
            "cycle" => Some(Pattern::Cycle),
            _ => None,
        }
    }
}

/// pulse 의 꺼진 구간이면 다음 켜짐까지 기다릴 ms, 켜진 구간이면 None.
/// 모든 워커가 같은 시작 시각을 기준으로 삼아 부하가 한꺼번에 켜지고 꺼진다.
pub fn pulse_wait(elapsed_ms: u64) -> Option<u64> {
    ((elapsed_ms / PULSE_MS) % 2 == 1).then(|| PULSE_MS - elapsed_ms % PULSE_MS)
}

/// cycle 패턴: 한 코어 차례의 최소 길이
pub const CYCLE_MIN_MS: u64 = 500;

/// cycle 패턴 창 길이: 실행 시간 안에 모든 코어가 두 번씩 돌게 나누되 최소 CYCLE_MIN_MS
pub fn cycle_window_ms(duration_ms: u64, threads: usize) -> u64 {
    (duration_ms / (threads.max(1) as u64 * 2)).max(CYCLE_MIN_MS)
}

/// cycle 패턴: 이 워커 차례가 아니면 다음 차례까지 기다릴 ms, 차례면 None.
/// 한 번에 한 코어만 돌아 그 코어가 최고 클럭까지 올라간다. 쉬던 코어는 차례마다 깨어난다.
pub fn cycle_wait(elapsed_ms: u64, window_ms: u64, cpu: usize, threads: usize) -> Option<u64> {
    let (w, n) = (elapsed_ms / window_ms, threads as u64);
    let ahead = (cpu as u64 + n - w % n) % n;
    (ahead != 0).then(|| (w + ahead) * window_ms - elapsed_ms)
}

/// 지금 동시에 계산 중인 워커 수 (불량 흉내용): cycle 은 한 번에 하나
pub fn active_threads(pattern: Pattern, threads: usize) -> usize {
    if pattern == Pattern::Cycle { 1 } else { threads }
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
    pub isa: Isa,
    pub seed: u64,
    pub expected: String,
    pub actual: String,
    pub block_start_ms: u64,
    pub at_ms: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct CpuOutcome {
    pub isa: Isa,
    pub rotate_isa: bool,
    pub kernels: KernelSet,
    pub pattern: Pattern,
    pub threads: usize,
    pub pinned: bool,
    pub blocks: u64,
    /// 워커별 블록 수의 최솟값 — 0 이면 검사 못 한 코어가 있다
    pub min_thread_blocks: u64,
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
    /// true 면 AVX-512 PC 에서 AVX2 도 번갈아 쓴다
    pub rotate_isa: bool,
    /// 검출 채점용 불량 모델 (라이브러리 전용)
    pub fault: Option<FaultInject>,
}

/// 정답표는 두 조건을 모두 만족할 때만 쓴다.
/// 1) 짧은 길이에서 스칼라(명세)와 선택 명령어 세트가 같다 (번갈아 쓸 `also` 도 함께)
/// 2) 실제 길이를 선택 명령어 세트로 두 번 계산해 같다 (스칼라 FMA 는 x86 에서 너무 느려서)
pub fn goldens(kernel: Kernel, isa: Isa, also: Option<Isa>, iters: u64) -> Option<Vec<u64>> {
    let agree = (0..GOLDEN_SEEDS).all(|s| {
        let spec = run_block(kernel, Isa::Scalar, s, SELF_CHECK_ITERS, None);
        std::iter::once(isa).chain(also).all(|i| run_block(kernel, i, s, SELF_CHECK_ITERS, None) == spec)
    });
    let once = || (0..GOLDEN_SEEDS).map(|s| run_block(kernel, isa, s, iters, None)).collect::<Vec<_>>();
    let (a, b) = (once(), once());
    golden_accepted(agree, &a, &b).then_some(a)
}

/// 정답표 채택 판정: 명세와 같고, 두 번 계산이 같아야 한다
fn golden_accepted(agree: bool, first: &[u64], second: &[u64]) -> bool {
    agree && first == second
}

/// 블록에 쓸 명령어 세트. 자동 선택된 AVX-512 PC 는 AVX2(256비트) 경로도 번갈아 쓴다 —
/// 256비트 경로에만 있는 불량이 보고됐다. 두 경로는 결과가 같아 정답표 하나로 비교한다.
pub fn block_isa(top: Isa, rotate: bool, avx2_ok: bool, block: u64, n_kernels: u64) -> Isa {
    if rotate && top == Isa::Avx512 && avx2_ok && (block / n_kernels) % 2 == 1 {
        Isa::Avx2
    } else {
        top
    }
}

/// 초당 계산량. 며칠짜리 실행에서 u64 곱셈이 넘치지 않게 u128 로 계산한다
fn per_sec(lane_iters: u64, run_ms: u64) -> u64 {
    if run_ms == 0 { 0 } else { (lane_iters as u128 * 1000 / run_ms as u128) as u64 }
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
    // 실제로 AVX2 를 번갈아 쓰는 경우만 true 로 보고하고, AVX2 도 명세와 대조한다
    let rotating = cfg.rotate_isa && cfg.isa == Isa::Avx512 && Isa::Avx2.supported();
    let mut tables = Vec::new();
    for kernel in cfg.kernels.kernels() {
        let iters = cfg.iters.unwrap_or(kernel.default_iters());
        match goldens(kernel, cfg.isa, rotating.then_some(Isa::Avx2), iters) {
            Some(gold) => tables.push(Table { kernel, iters, gold }),
            None => {
                return CpuOutcome {
                    isa: cfg.isa, rotate_isa: rotating, kernels: cfg.kernels, pattern: cfg.pattern, threads: cfg.threads, pinned: false,
                    blocks: 0, min_thread_blocks: 0, lane_iters: 0, run_ms: 0, lane_iters_per_sec: 0,
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
    let avx2_ok = Isa::Avx2.supported();
    let window = cycle_window_ms(cfg.duration.as_millis() as u64, cfg.threads);

    let handles: Vec<_> = (0..cfg.threads)
        .map(|cpu| {
            let (tables, stop, first_error) = (tables.clone(), stop.clone(), first_error.clone());
            let (isa, pattern, inject, fault, threads) = (cfg.isa, cfg.pattern, cfg.inject, cfg.fault, cfg.threads);
            let rotate_isa = cfg.rotate_isa;
            std::thread::spawn(move || {
                let pinned = crate::affinity::pin_current_thread(cpu);
                let mut block = 0u64;
                let mut lane_iters = 0u64;
                while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                    let elapsed = run_start.elapsed().as_millis() as u64;
                    let wait = match pattern {
                        Pattern::Pulse => pulse_wait(elapsed),
                        Pattern::Cycle => cycle_wait(elapsed, window, cpu, threads),
                        Pattern::Steady => None,
                    };
                    if let Some(w) = wait {
                        let mut nap = Duration::from_millis(w);
                        // cycle 은 다음 차례가 멀 수 있어 마감을 넘겨 자지 않는다
                        if pattern == Pattern::Cycle {
                            nap = nap.min(deadline.saturating_duration_since(Instant::now()));
                        }
                        std::thread::sleep(nap);
                        continue;
                    }
                    // 블록마다 커널을 돌아가며 쓰고, 코어마다 시드를 어긋나게 한다
                    let t = &tables[(block % tables.len() as u64) as usize];
                    let seed = (block + cpu as u64) % GOLDEN_SEEDS;
                    let flip = inject.filter(|i| i.cpu == cpu && i.block == block).map(|i| i.flip);
                    let bisa = block_isa(isa, rotate_isa, avx2_ok, block, tables.len() as u64);
                    let block_start = Instant::now();
                    let got = match fault.filter(|f| f.cpu == cpu && f.block == block) {
                        Some(f) => crate::fault::run_faulty(f.model, t.kernel, bisa, seed, t.iters, active_threads(pattern, threads)),
                        None => run_block(t.kernel, bisa, seed, t.iters, flip),
                    };
                    let want = t.gold[seed as usize];
                    if got != want {
                        let mut slot = first_error.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(CpuError {
                                cpu, block, kernel: t.kernel, isa: bisa, seed,
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
    let mut min_thread_blocks = u64::MAX;
    let mut lane_iters = 0;
    let mut pinned = true;
    for h in handles {
        let (p, b, l) = h.join().expect("워커 스레드가 죽었다");
        pinned &= p;
        blocks += b;
        min_thread_blocks = min_thread_blocks.min(b);
        lane_iters += l;
    }
    // 스레드가 없으면 0
    if cfg.threads == 0 {
        min_thread_blocks = 0;
    }
    let run_ms = run_start.elapsed().as_millis() as u64;
    let lane_iters_per_sec = per_sec(lane_iters, run_ms);
    let error = first_error.lock().unwrap().clone();
    CpuOutcome {
        isa: cfg.isa, rotate_isa: rotating, kernels: cfg.kernels, pattern: cfg.pattern, threads: cfg.threads, pinned, blocks,
        min_thread_blocks, lane_iters, run_ms, lane_iters_per_sec, elapsed_ms: ms(Instant::now()), golden_unstable: false, error,
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
            pattern: Pattern::Steady, iters: Some(1 << 12), inject, rotate_isa: false, fault: None,
        }
    }

    #[test]
    fn block_isa_rotation() {
        // mix 네 블록마다 AVX-512 ↔ AVX2
        for (block, want) in [(0, Isa::Avx512), (3, Isa::Avx512), (4, Isa::Avx2), (7, Isa::Avx2), (8, Isa::Avx512)] {
            assert_eq!(block_isa(Isa::Avx512, true, true, block, 4), want, "block={block}");
        }
        // 지정했거나(rotate=false), AVX2 가 없거나, 최고가 AVX-512 가 아니면 그대로
        assert_eq!(block_isa(Isa::Avx512, false, true, 4, 4), Isa::Avx512);
        assert_eq!(block_isa(Isa::Avx512, true, false, 4, 4), Isa::Avx512);
        assert_eq!(block_isa(Isa::Avx2, true, true, 4, 4), Isa::Avx2);
        assert_eq!(block_isa(Isa::Scalar, true, true, 4, 4), Isa::Scalar);
    }

    #[test]
    fn goldens_are_stable() {
        for k in Kernel::ALL {
            let g = goldens(k, Isa::best(), None, 1 << 12).expect("정답표가 두 번 같아야 한다");
            assert_eq!(g.len(), GOLDEN_SEEDS as usize);
            // 정답표는 명세(스칼라)와도 같아야 한다
            assert_eq!(g[5], run_block(k, Isa::Scalar, 5, 1 << 12, None), "{k:?}");
        }
    }

    // 번갈아 쓸 명령어 세트도 명세와 대조한다. 같으면 정답표는 그대로 채택된다
    #[test]
    fn goldens_also_checks_second_isa() {
        for k in Kernel::ALL {
            let base = goldens(k, Isa::best(), None, 1 << 12);
            assert_eq!(goldens(k, Isa::best(), Some(Isa::Scalar), 1 << 12), base, "{k:?}");
            if Isa::Avx2.supported() {
                assert_eq!(goldens(k, Isa::best(), Some(Isa::Avx2), 1 << 12), base, "{k:?}");
            }
        }
    }

    // rotate_isa 는 실제로 AVX2 를 번갈아 쓸 때만 true 로 보고한다
    #[test]
    fn rotate_isa_reported_only_when_rotating() {
        let out = run(&CpuConfig { rotate_isa: true, duration: Duration::from_millis(200), ..cfg(KernelSet::Chain, None) });
        assert_eq!(out.rotate_isa, Isa::best() == Isa::Avx512 && Isa::Avx2.supported());
        assert!(!out.failed(), "{:?}", out.error);
        assert!(!run(&CpuConfig { duration: Duration::from_millis(200), ..cfg(KernelSet::Chain, None) }).rotate_isa);
    }

    // 변이 테스트 보강: 두 조건 중 하나라도 어긋나면 정답표를 버린다
    #[test]
    fn golden_accepted_needs_both() {
        assert!(golden_accepted(true, &[1, 2], &[1, 2]));
        assert!(!golden_accepted(true, &[1, 2], &[1, 3]));
        assert!(!golden_accepted(false, &[1, 2], &[1, 2]));
        assert!(!golden_accepted(false, &[1, 2], &[1, 3]));
    }

    #[test]
    fn clean_run_has_no_error() {
        for set in [KernelSet::Chain, KernelSet::Wide, KernelSet::Fma, KernelSet::Fma32, KernelSet::Mix] {
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

    // mix 는 블록 번호 순서대로 chain → wide → fma → fma32 를 쓴다
    #[test]
    fn mix_rotates_kernels_by_block() {
        for (block, want) in [(0, Kernel::Chain), (1, Kernel::Wide), (2, Kernel::Fma), (3, Kernel::Fma32), (4, Kernel::Chain), (6, Kernel::Fma)] {
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

    // 변이 테스트 보강: pulse 는 꺼진 구간에 정말 쉰다.
    // 480ms 실행이면 250~480ms 사이에 꺼짐 → 500ms 까지 잠든 뒤 끝나므로 실행 시간이 500ms 이상이다
    #[test]
    fn pulse_sleeps_through_off_window() {
        let out = run(&CpuConfig {
            threads: 1, duration: Duration::from_millis(480), pattern: Pattern::Pulse, ..cfg(KernelSet::Chain, None)
        });
        assert!(out.run_ms >= 2 * PULSE_MS, "run_ms={}", out.run_ms);
    }

    #[test]
    fn cycle_window_lengths() {
        assert_eq!(cycle_window_ms(60_000, 16), 1_875);
        assert_eq!(cycle_window_ms(10_000, 4), 1_250);
        assert_eq!(cycle_window_ms(1_000, 16), CYCLE_MIN_MS);
        assert_eq!(cycle_window_ms(1_000, 0), CYCLE_MIN_MS);
    }

    #[test]
    fn cycle_wait_turns() {
        // 창 1000ms, 4스레드: 코어 0 → 1 → 2 → 3 → 0 …
        assert_eq!(cycle_wait(0, 1000, 0, 4), None);
        assert_eq!(cycle_wait(0, 1000, 1, 4), Some(1000));
        assert_eq!(cycle_wait(999, 1000, 1, 4), Some(1));
        assert_eq!(cycle_wait(1000, 1000, 1, 4), None);
        assert_eq!(cycle_wait(0, 1000, 3, 4), Some(3000));
        assert_eq!(cycle_wait(3500, 1000, 0, 4), Some(500));
        assert_eq!(cycle_wait(4000, 1000, 0, 4), None);
        assert_eq!(cycle_wait(250, 1000, 0, 1), None);
    }

    #[test]
    fn active_threads_by_pattern() {
        assert_eq!(active_threads(Pattern::Cycle, 8), 1);
        assert_eq!(active_threads(Pattern::Steady, 8), 8);
        assert_eq!(active_threads(Pattern::Pulse, 8), 8);
    }

    #[test]
    fn cycle_run_covers_every_thread() {
        let out = run(&CpuConfig {
            threads: 3, duration: Duration::from_millis(3000), pattern: Pattern::Cycle, ..cfg(KernelSet::Mix, None)
        });
        assert!(!out.failed(), "{:?}", out.error);
        assert!(out.min_thread_blocks >= 1, "min_thread_blocks={}", out.min_thread_blocks);
    }

    #[test]
    fn kernel_set_parse() {
        assert_eq!(KernelSet::parse("mix"), Some(KernelSet::Mix));
        assert_eq!(KernelSet::parse("fma"), Some(KernelSet::Fma));
        assert_eq!(KernelSet::parse("fma32"), Some(KernelSet::Fma32));
        assert_eq!(KernelSet::parse("avx"), None);
        assert_eq!(KernelSet::Mix.kernels(), Kernel::ALL.to_vec());
        assert_eq!(Pattern::parse("steady"), Some(Pattern::Steady));
        assert_eq!(Pattern::parse("pulse"), Some(Pattern::Pulse));
        assert_eq!(Pattern::parse("cycle"), Some(Pattern::Cycle));
        assert_eq!(Pattern::parse("burst"), None);
    }

    // 긴 실행에서도 초당 계산량이 넘치지 않아야 한다
    #[test]
    fn per_sec_does_not_overflow() {
        assert_eq!(per_sec(0, 0), 0);
        assert_eq!(per_sec(3000, 1000), 3000);
        assert_eq!(per_sec(u64::MAX / 10, 1_000_000), (u64::MAX / 10) / 1000);
    }
}
