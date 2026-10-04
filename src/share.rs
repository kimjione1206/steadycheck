//! 코어끼리 주고받기 검사: 일꾼마다 우편함 하나에 보내고, 앞 일꾼의 우편함에서 받아 값을 대조한다.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 우편함 한 칸(64바이트 캐시 줄) 안의 데이터 단어 수
pub const WORDS: usize = 7;

/// seq 가 2s 이면 비어 있고 다음 순번 s, 2s+1 이면 순번 s 가 차 있다
#[repr(C, align(64))]
struct Mailbox {
    seq: AtomicU64,
    data: [AtomicU64; WORDS],
}

/// 받는 일꾼 cpu 의 msg 번째 받기(0부터)에서 옛 값을 읽은 것처럼 흉내 낸다
#[derive(Clone, Copy, Debug)]
pub struct ShareInject {
    pub cpu: usize,
    pub msg: u64,
}

pub struct ShareConfig {
    pub threads: usize,
    pub duration: Duration,
    pub inject: Option<ShareInject>,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub struct ShareError {
    pub cpu: usize,
    pub from: usize,
    pub seq: u64,
    pub word: usize,
    pub expected: String,
    pub actual: String,
    pub at_ms: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct ShareOutcome {
    pub threads: usize,
    pub pinned: bool,
    pub messages: u64,
    pub min_thread_messages: u64,
    pub counter_ok: bool,
    pub messages_per_sec: u64,
    pub elapsed_ms: u64,
    pub error: Option<ShareError>,
}

impl ShareOutcome {
    pub fn failed(&self) -> bool {
        self.error.is_some() || !self.counter_ok
    }
}

/// 보낸 일꾼·순번·단어 번호로 정해지는 값
pub fn payload(from: usize, seq: u64, j: usize) -> u64 {
    crate::kernel::splitmix64(seq ^ ((from as u64) << 48) ^ ((j as u64) << 40))
}

struct WorkerOut {
    pinned: bool,
    received: u64,
    error: Option<ShareError>,
}

pub fn run(cfg: &ShareConfig) -> ShareOutcome {
    let start = Instant::now();
    let n = cfg.threads.max(1);
    let boxes: Vec<Mailbox> = (0..n)
        .map(|_| Mailbox { seq: AtomicU64::new(0), data: std::array::from_fn(|_| AtomicU64::new(0)) })
        .collect();
    let stop = AtomicBool::new(false);
    let counter = AtomicU64::new(0);
    let outs: Vec<WorkerOut> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..n)
            .map(|k| {
                let (boxes, stop, counter) = (&boxes, &stop, &counter);
                s.spawn(move || worker(cfg, start, stop, counter, boxes, k))
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("주고받기 일꾼이 죽었다")).collect()
    });
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let messages: u64 = outs.iter().map(|o| o.received).sum();
    ShareOutcome {
        threads: n,
        pinned: outs.iter().all(|o| o.pinned),
        messages,
        min_thread_messages: outs.iter().map(|o| o.received).min().unwrap_or(0),
        // 공용 카운터의 원자적 더하기가 하나라도 빠지거나 겹치면 합이 어긋난다
        counter_ok: counter.load(Ordering::Acquire) == messages,
        messages_per_sec: crate::cpu::per_sec(messages, elapsed_ms),
        elapsed_ms,
        // 여러 일꾼이 동시에 틀리면 가장 먼저 잡은 것
        error: outs.into_iter().filter_map(|o| o.error).min_by_key(|e| e.at_ms),
    }
}

/// 일꾼 k: 우편함 k 에 보내고, 앞 일꾼의 우편함 (k+n-1)%n 에서 받는다
fn worker(cfg: &ShareConfig, start: Instant, stop: &AtomicBool, counter: &AtomicU64, boxes: &[Mailbox], k: usize) -> WorkerOut {
    let pinned = crate::affinity::pin_current_thread(k);
    let n = boxes.len();
    let from = (k + n - 1) % n;
    let (out_box, in_box) = (&boxes[k], &boxes[from]);
    let inject_at = cfg.inject.filter(|j| j.cpu == k).map(|j| j.msg);
    let mut received = 0u64;
    let mut spins = 0u64;
    while !stop.load(Ordering::Relaxed) {
        if spins.is_multiple_of(1024) && start.elapsed() >= cfg.duration {
            break;
        }
        spins += 1;
        let mut busy = false;
        // 보내기: 비어 있으면 순번 s 의 값을 채우고 찼다고 표시
        let q = out_box.seq.load(Ordering::Acquire);
        if q % 2 == 0 {
            let s = q / 2;
            for (j, d) in out_box.data.iter().enumerate() {
                d.store(payload(k, s, j), Ordering::Relaxed);
            }
            out_box.seq.store(q + 1, Ordering::Release);
            busy = true;
        }
        // 받기: 차 있으면 값을 대조하고 비었다고 표시
        let q = in_box.seq.load(Ordering::Acquire);
        if q % 2 == 1 {
            let s = q / 2;
            for (j, d) in in_box.data.iter().enumerate() {
                let mut got = d.load(Ordering::Relaxed);
                // 주입: 직전 순번의 값을 읽은 것처럼
                if j == 0 && inject_at == Some(received) {
                    got = payload(from, s.wrapping_sub(1), 0);
                }
                let want = payload(from, s, j);
                if got != want {
                    stop.store(true, Ordering::Relaxed);
                    let error = ShareError {
                        cpu: k, from, seq: s, word: j,
                        expected: format!("{want:#018x}"),
                        actual: format!("{got:#018x}"),
                        at_ms: start.elapsed().as_millis() as u64,
                    };
                    return WorkerOut { pinned, received, error: Some(error) };
                }
            }
            in_box.seq.store(q + 1, Ordering::Release);
            received += 1;
            counter.fetch_add(1, Ordering::AcqRel);
            busy = true;
        }
        if !busy {
            std::hint::spin_loop();
        }
    }
    WorkerOut { pinned, received, error: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_known() {
        // 정답은 splitmix64 정의(더하기 0x9E3779B97F4A7C15, 섞기 두 번)를 파이썬으로 따로 계산한 값
        assert_eq!(payload(0, 0, 0), 0xE220_A839_7B1D_CDAF);
        assert_eq!(payload(3, 5, 2), 0x4F51_1F3F_B2D0_18AF);
        assert_eq!(payload(1, 4, 6), 0xBDDB_06D7_203D_777F);
        // 첫 받기 주입이 쓰는 감긴 순번(u64::MAX): 순번 비트가 일꾼·단어 비트와 겹친다
        assert_eq!(payload(1, u64::MAX, 3), 0xE6CF_372B_A0F4_BC4A);
    }

    #[test]
    fn mailbox_is_one_cache_line() {
        assert_eq!((std::mem::size_of::<Mailbox>(), std::mem::align_of::<Mailbox>()), (64, 64));
    }

    #[test]
    fn clean_run_passes() {
        // 진행만 보는 시험이라 마감을 넉넉히: 다른 시험과 겹친 윈도우 러너(4코어)에서는 고정된 일꾼의 차례가 0.5초 넘게 밀린 적이 있다
        let out = run(&ShareConfig { threads: 4, duration: Duration::from_secs(3), inject: None });
        assert!(out.error.is_none(), "{:?}", out.error);
        assert!(out.counter_ok && !out.failed());
        assert_eq!(out.threads, 4);
        assert!(out.min_thread_messages >= 1);
        assert!(out.messages_per_sec > 0);
    }

    #[test]
    fn single_thread_works() {
        // 진행만 보는 시험이라 마감을 넉넉히 (clean_run_passes 와 같은 까닭)
        let out = run(&ShareConfig { threads: 1, duration: Duration::from_secs(2), inject: None });
        assert!(!out.failed(), "{:?}", out.error);
        assert_eq!(out.threads, 1);
        assert!(out.min_thread_messages >= 1);
        assert_eq!(out.messages, out.min_thread_messages);
    }

    #[test]
    fn injected_stale_read_is_caught() {
        let out = run(&ShareConfig { threads: 4, duration: Duration::from_secs(5), inject: Some(ShareInject { cpu: 1, msg: 5 }) });
        let e = out.error.clone().expect("주입한 옛 값을 잡아야 한다");
        assert_eq!((e.cpu, e.from, e.seq, e.word), (1, 0, 5, 0));
        // 기대값은 순번 5, 실제는 직전 순번 4 의 값
        assert_eq!(e.expected, format!("{:#018x}", payload(0, 5, 0)));
        assert_eq!(e.actual, format!("{:#018x}", payload(0, 4, 0)));
        assert!(out.failed());
    }

    // 일꾼 수가 2의 거듭제곱이 아니어도 일꾼 0 은 마지막 일꾼에게서 받는다 (자기 우편함이 아니라)
    #[test]
    fn three_threads_receive_from_previous_worker() {
        let out = run(&ShareConfig { threads: 3, duration: Duration::from_secs(5), inject: Some(ShareInject { cpu: 0, msg: 2 }) });
        let e = out.error.expect("주입한 옛 값을 잡아야 한다");
        assert_eq!((e.cpu, e.from, e.seq, e.word), (0, 2, 2, 0));
    }

    #[test]
    fn stale_read_on_first_message_is_caught() {
        // 0 번째 받기: 직전 순번은 u64::MAX 로 감긴다
        let out = run(&ShareConfig { threads: 1, duration: Duration::from_secs(5), inject: Some(ShareInject { cpu: 0, msg: 0 }) });
        let e = out.error.expect("첫 받기 주입을 잡아야 한다");
        assert_eq!((e.cpu, e.from, e.seq, e.word), (0, 0, 0, 0));
        assert_eq!(out.messages, 0);
    }
}
