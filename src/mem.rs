//! RAM 검사: 여러 일꾼이 버퍼를 나눠 맡아, 패스마다 쓰기 → 읽기+뒤집어 쓰기 → 뒤집은 값 읽기로 대조한다. 6가지 패턴을 돌아가며 반복.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct MemInject {
    pub pass: u64,
    pub word: usize,
    pub bit: u32,
    /// true 면 2단계 뒤(뒤집은 값이 들었을 때) 넣는다
    pub late: bool,
}

/// 검출 능력 채점 전용: 실제 메모리 불량을 흉내 낸 모델
#[derive(Clone, Copy, Debug)]
pub enum MemFault {
    /// 동시에 메모리를 검사하는 일꾼이 min_active 이상일 때만 그 칸을 읽으면 비트가 틀린다 —
    /// 메모리 빠른 설정·메모리 컨트롤러가 전송량이 높을 때만 불안정한 경우 (메모리 자체 값은 멀쩡)
    BusyOnly { word: usize, bit: u32, min_active: usize },
    /// 칸 word 의 비트가 0 → 1 로 바뀌면 뒤쪽 칸 word + distance 의 같은 비트가 1 이 된다 — 이웃 칸 간섭
    CouplingUp { word: usize, distance: usize, bit: u32 },
}

#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub struct MemError {
    pub thread: usize,
    pub pass: u64,
    pub pattern: String,
    pub offset_bytes: usize,
    pub expected: String,
    pub actual: String,
    pub reread: String,
    pub pass_start_ms: u64,
    pub at_ms: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct MemOutcome {
    pub bytes: usize,
    pub threads: usize,
    pub pinned: bool,
    pub passes: u64,
    pub min_thread_passes: u64,
    pub bytes_verified: u64,
    pub verified_bytes_per_sec: u64,
    pub elapsed_ms: u64,
    pub error: Option<MemError>,
}

impl MemOutcome {
    pub fn failed(&self) -> bool {
        self.error.is_some()
    }
}

pub struct MemConfig {
    pub mb: usize,
    pub duration: Duration,
    pub threads: usize,
    pub inject: Option<MemInject>,
    pub fault: Option<MemFault>,
}

#[derive(Clone, Copy)]
enum Pattern {
    Solid(u64),
    Address(u64),
    Random(u64),
}

// 패스 순서: 0101.., 1010.., 전부 0, 전부 1, 주소 섞기, 무작위 — 반복
fn pattern_for(pass: u64) -> Pattern {
    match pass % 6 {
        0 => Pattern::Solid(0x5555_5555_5555_5555),
        1 => Pattern::Solid(0xAAAA_AAAA_AAAA_AAAA),
        2 => Pattern::Solid(0),
        3 => Pattern::Solid(u64::MAX),
        4 => Pattern::Address(0xD1B5_4A32_D192_ED03 ^ pass),
        _ => Pattern::Random(crate::kernel::splitmix64(0x2545_F491_4F6C_DD1D ^ pass)),
    }
}

fn value(p: Pattern, i: usize) -> u64 {
    match p {
        Pattern::Solid(v) => v,
        Pattern::Address(k) => (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ k,
        Pattern::Random(k) => crate::kernel::splitmix64(k.wrapping_add(i as u64)),
    }
}

fn name(p: Pattern) -> String {
    match p {
        Pattern::Solid(v) => format!("solid {v:#018x}"),
        Pattern::Address(_) => "address".into(),
        Pattern::Random(_) => "random".into(),
    }
}

/// 칸 i 에 v 를 쓴다. couple = (가해 칸, 거리, 비트): 그 칸의 비트가 0 → 1 이면 뒤쪽 칸에 번진다
#[inline(always)]
unsafe fn store(ptr: *mut u64, len: usize, i: usize, v: u64, couple: Option<(usize, usize, u64)>) {
    if let Some((at, dist, mask)) = couple {
        if i == at && !ptr.add(i).read_volatile() & v & mask != 0 && dist < len - at {
            let q = ptr.add(at + dist);
            q.write_volatile(q.read_volatile() | mask);
        }
    }
    ptr.add(i).write_volatile(v);
}

/// 칸 i 를 읽는다. busy = (칸, 비트): 그 칸을 읽으면 비트가 틀린다 (메모리 값은 그대로)
#[inline(always)]
unsafe fn load(ptr: *const u64, i: usize, busy: Option<(usize, u64)>) -> u64 {
    let got = ptr.add(i).read_volatile();
    match busy {
        Some((at, mask)) if i == at => got ^ mask,
        _ => got,
    }
}

/// 버퍼를 일꾼 수만큼 나눈 조각들의 시작 칸. 나머지는 마지막 일꾼이 맡는다
pub fn chunk_starts(words: usize, threads: usize) -> Vec<usize> {
    let per = words / threads;
    (0..threads).map(|t| t * per).collect()
}

struct WorkerOut {
    pinned: bool,
    passes: u64,
    verified: u64,
    error: Option<MemError>,
}

pub fn run(cfg: &MemConfig) -> MemOutcome {
    let start = Instant::now();
    let words = cfg.mb * 1024 * 1024 / 8;
    let threads = cfg.threads.clamp(1, words.max(1));
    let starts = chunk_starts(words, threads);
    let mut buf = vec![0u64; words];
    let stop = AtomicBool::new(false);
    let outs: Vec<WorkerOut> = std::thread::scope(|s| {
        let mut rest: &mut [u64] = &mut buf;
        let mut handles = Vec::new();
        for t in 0..threads {
            let len = if t + 1 == threads { rest.len() } else { starts[t + 1] - starts[t] };
            let (chunk, tail) = rest.split_at_mut(len);
            rest = tail;
            let (base, stop) = (starts[t], &stop);
            handles.push(s.spawn(move || worker(cfg, start, stop, t, threads, base, chunk)));
        }
        handles.into_iter().map(|h| h.join().expect("메모리 일꾼이 죽었다")).collect()
    });
    drop(buf);
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let bytes_verified = outs.iter().map(|o| o.verified).sum();
    MemOutcome {
        bytes: words * 8,
        threads,
        pinned: outs.iter().all(|o| o.pinned),
        passes: outs.iter().map(|o| o.passes).sum(),
        min_thread_passes: outs.iter().map(|o| o.passes).min().unwrap_or(0),
        bytes_verified,
        verified_bytes_per_sec: crate::cpu::per_sec(bytes_verified, elapsed_ms),
        elapsed_ms,
        // 여러 일꾼이 동시에 틀리면 가장 먼저 잡은 것
        error: outs.into_iter().filter_map(|o| o.error).min_by_key(|e| e.at_ms),
    }
}

/// 일꾼 t: 버퍼 전체의 칸 base.. 에 해당하는 자기 조각을 반복 검사한다
fn worker(cfg: &MemConfig, start: Instant, stop: &AtomicBool, t: usize, threads: usize, base: usize, chunk: &mut [u64]) -> WorkerOut {
    let pinned = crate::affinity::pin_current_thread(t);
    let ms = || start.elapsed().as_millis() as u64;
    let n = chunk.len();
    let ptr = chunk.as_mut_ptr();
    // 버퍼 전체 칸 번호 → 내 조각 안의 번호
    let local = |w: usize| (w >= base && w < base + n).then(|| w - base);
    // 일꾼은 모두 동시에 돈다
    let active = threads;
    let couple = match cfg.fault {
        Some(MemFault::CouplingUp { word, distance, bit }) => local(word).map(|at| (at, distance, 1u64 << (bit % 64))),
        _ => None,
    };
    let busy = match cfg.fault {
        Some(MemFault::BusyOnly { word, bit, min_active }) if active >= min_active => local(word).map(|at| (at, 1u64 << (bit % 64))),
        _ => None,
    };
    let fail = |pass: u64, p: Pattern, i: usize, want: u64, got: u64, pass_start_ms: u64| MemError {
        thread: t,
        pass,
        pattern: name(p),
        offset_bytes: (base + i) * 8,
        expected: format!("{want:#018x}"),
        actual: format!("{got:#018x}"),
        reread: format!("{:#018x}", unsafe { ptr.add(i).read_volatile() }),
        pass_start_ms,
        at_ms: ms(),
    };
    let mut pass = 0u64;
    let mut verified = 0u64;
    while start.elapsed() < cfg.duration && !stop.load(Ordering::Relaxed) {
        let p = pattern_for(pass);
        let pass_start_ms = ms();
        let flip_at = |late: bool| cfg.inject.filter(|j| j.pass == pass && j.late == late).and_then(|j| local(j.word).map(|w| (w, j.bit)));
        let flip = |at: Option<(usize, u32)>| {
            if let Some((w, bit)) = at {
                unsafe {
                    let q = ptr.add(w);
                    q.write_volatile(q.read_volatile() ^ (1u64 << (bit % 64)));
                }
            }
        };
        // volatile: 컴파일러가 "쓴 값을 그대로 안다"며 읽기를 생략하지 못하게
        // 1단계: 차례로 쓴다
        for i in 0..n {
            unsafe { store(ptr, n, i, value(p, base + i), couple) }
        }
        flip(flip_at(false));
        // 2단계: 읽어 대조하고 그 자리에 뒤집은 값을 쓴다 — 읽기와 쓰기가 섞여 메모리 길이 계속 방향을 바꾼다
        for i in 0..n {
            let want = value(p, base + i);
            let got = unsafe { load(ptr, i, busy) };
            if got != want {
                stop.store(true, Ordering::Relaxed);
                return WorkerOut { pinned, passes: pass + 1, verified: verified + i as u64 * 8, error: Some(fail(pass, p, i, want, got, pass_start_ms)) };
            }
            unsafe { store(ptr, n, i, !want, couple) }
        }
        flip(flip_at(true));
        // 3단계: 뒤집은 값을 읽어 대조한다
        for i in 0..n {
            let want = !value(p, base + i);
            let got = unsafe { load(ptr, i, busy) };
            if got != want {
                stop.store(true, Ordering::Relaxed);
                return WorkerOut { pinned, passes: pass + 1, verified: verified + n as u64 * 8, error: Some(fail(pass, p, i, want, got, pass_start_ms)) };
            }
        }
        verified += n as u64 * 8;
        pass += 1;
    }
    WorkerOut { pinned, passes: pass, verified, error: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn clean_run_has_no_error() {
        let out = run(&MemConfig { mb: 8, duration: Duration::from_millis(500), threads: 1, inject: None, fault: None });
        assert!(out.error.is_none(), "{:?}", out.error);
        assert!(out.passes >= 1);
        assert_eq!(out.bytes_verified, out.passes * out.bytes as u64);
    }

    #[test]
    fn injected_flip_is_caught_at_exact_place() {
        let inj = MemInject { pass: 2, word: 12_345, bit: 17, late: false };
        let out = run(&MemConfig { mb: 8, duration: Duration::from_secs(5), threads: 1, inject: Some(inj), fault: None });
        let e = out.error.clone().expect("주입한 오류를 잡아야 한다");
        assert_eq!((e.pass, e.offset_bytes), (2, 12_345 * 8));
        assert_ne!(e.expected, e.actual);
        assert_eq!(e.actual, e.reread, "뒤집힌 값은 다시 읽어도 같아야 한다");
        assert!(out.failed());
    }

    // 변이 테스트 보강: 오류 보고의 숫자가 정확해야 한다 (패스 3 = 전부 1)
    #[test]
    fn error_report_numbers_are_exact() {
        let bytes = 8 * 1024 * 1024;
        let inj = MemInject { pass: 3, word: 12_345, bit: 17, late: false };
        let out = run(&MemConfig { mb: 8, duration: Duration::from_secs(5), threads: 1, inject: Some(inj), fault: None });
        assert_eq!((out.bytes, out.passes), (bytes, 4));
        assert_eq!(out.bytes_verified, 3 * bytes as u64 + 12_345 * 8);
        let e = out.error.expect("주입한 오류를 잡아야 한다");
        assert_eq!(e.expected, format!("{:#018x}", u64::MAX));
        assert_eq!(e.actual, format!("{:#018x}", u64::MAX ^ (1 << 17)));
    }

    // 변이 테스트 보강: 버퍼 밖 위치의 주입은 무시한다 (버퍼 밖에 쓰면 안 된다)
    #[test]
    fn out_of_range_injection_is_ignored() {
        let words = 8 * 1024 * 1024 / 8;
        let inj = MemInject { pass: 0, word: words, bit: 0, late: false };
        let out = run(&MemConfig { mb: 8, duration: Duration::from_millis(300), threads: 1, inject: Some(inj), fault: None });
        assert!(out.error.is_none(), "{:?}", out.error);
    }

    // 변이 테스트 보강: 패턴 값은 정답표와 같아야 한다
    #[test]
    fn pattern_values_are_known() {
        let solid = [0x5555_5555_5555_5555, 0xAAAA_AAAA_AAAA_AAAA, 0, u64::MAX];
        for pass in [0, 1, 2, 3, 6, 7, 8, 9] {
            assert_eq!(value(pattern_for(pass), 1000), solid[pass as usize % 6], "pass={pass}");
        }
        assert_eq!(value(pattern_for(4), 0), 0xD1B5_4A32_D192_ED07);
        assert_eq!(value(pattern_for(4), 1000), 0xD906_36AB_EB66_5F0F);
        assert_eq!(value(pattern_for(10), 1), 0x4F82_338B_AED8_911C);
        // 무작위: 패스마다 씨앗이 다르다
        assert_eq!(value(pattern_for(5), 0), 0x328D_4957_968F_4938);
        assert_eq!(value(pattern_for(5), 1000), 0x5029_0CA7_4559_6655);
        assert_eq!(value(pattern_for(11), 1), 0xFCD9_434E_3CE6_06F0);
        // 패스 11 은 패스 5 의 수열을 몇 칸 민 것이 아니다
        assert_ne!(value(pattern_for(11), 2), value(pattern_for(5), 0));
        assert_eq!(name(pattern_for(5)), "random");
    }

    #[test]
    fn late_flip_is_caught_in_complement_stage() {
        // 2단계 뒤에 넣으면 3단계(뒤집은 값 읽기)에서 잡힌다: 기대값은 뒤집은 값
        let inj = MemInject { pass: 3, word: 12_345, bit: 17, late: true };
        let out = run(&MemConfig { mb: 8, duration: Duration::from_secs(5), threads: 1, inject: Some(inj), fault: None });
        let e = out.error.expect("늦은 주입을 잡아야 한다");
        assert_eq!((e.pass, e.offset_bytes), (3, 12_345 * 8));
        assert_eq!((e.expected.as_str(), e.actual), ("0x0000000000000000", format!("{:#018x}", 1u64 << 17)));
        // 2단계는 다 끝났으므로 이 패스 조각 전체를 센다
        assert_eq!(out.bytes_verified, 4 * (8u64 << 20));
    }

    #[test]
    fn coupling_spreads_only_on_rising_bit() {
        // 버퍼는 앞 4칸만 쓰고, 그 뒤 칸은 버퍼 밖에 써 버리는지 지켜본다
        let mut b = [0u64; 12];
        let p = b.as_mut_ptr();
        let c = Some((1, 2, 1u64 << 3));
        unsafe {
            store(p, 4, 1, 0b0001, c); // 비트 3 은 그대로 0 → 번지지 않음
            assert_eq!(p.add(3).read(), 0);
            store(p, 4, 1, 0b1001, c); // 비트 3 이 0 → 1 → 칸 3 에 번짐
            assert_eq!((p.add(1).read(), p.add(3).read()), (0b1001, 0b1000));
            p.add(3).write(0);
            store(p, 4, 1, 0b1001, c); // 이미 1 → 1: 번지지 않음
            assert_eq!(p.add(3).read(), 0);
            store(p, 4, 2, 0b1000, c); // 가해 칸이 아니면 번지지 않음
            assert_eq!(p.add(3).read(), 0);
            let far = Some((1, 9, 1u64 << 3));
            store(p, 4, 1, 0, far);
            store(p, 4, 1, 0b1000, far); // 피해 칸이 버퍼 밖이면 아무것도 안 함
        }
        assert_eq!(b, [0, 0b1000, 0b1000, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn chunks_split_evenly_with_rest_on_last() {
        assert_eq!(chunk_starts(10, 3), vec![0, 3, 6]);
        assert_eq!(chunk_starts(8, 4), vec![0, 2, 4, 6]);
        assert_eq!(chunk_starts(5, 1), vec![0]);
    }

    #[test]
    fn many_workers_cover_their_chunks() {
        let out = run(&MemConfig { mb: 8, duration: Duration::from_millis(500), threads: 4, inject: None, fault: None });
        assert!(out.error.is_none(), "{:?}", out.error);
        assert_eq!((out.threads, out.bytes), (4, 8 << 20));
        assert!(out.min_thread_passes >= 1);
        assert!(out.passes >= 4 * out.min_thread_passes);
        assert_eq!(out.bytes_verified % (2 << 20), 0, "조각(2MB) 단위로 셈");
        assert!(out.verified_bytes_per_sec > 0);
    }

    #[test]
    fn error_in_last_chunk_names_its_worker() {
        // 8MB = 1,048,576 칸, 일꾼 3명 → 시작 0 / 349,525 / 699,050, 마지막 칸은 일꾼 2
        let words = 8 * 1024 * 1024 / 8;
        let inj = MemInject { pass: 1, word: words - 1, bit: 63, late: false };
        let out = run(&MemConfig { mb: 8, duration: Duration::from_secs(5), threads: 3, inject: Some(inj), fault: None });
        let e = out.error.expect("마지막 칸 주입을 잡아야 한다");
        assert_eq!((e.thread, e.pass, e.offset_bytes), (2, 1, (words - 1) * 8));
    }

    #[test]
    fn zero_workers_becomes_one() {
        let out = run(&MemConfig { mb: 1, duration: Duration::from_millis(100), threads: 0, inject: None, fault: None });
        assert_eq!(out.threads, 1);
    }

    #[test]
    fn busy_flip_changes_only_that_word() {
        let b = [7u64; 4];
        let p = b.as_ptr();
        unsafe {
            assert_eq!(load(p, 2, Some((2, 1 << 5))), 7 ^ (1 << 5));
            assert_eq!(load(p, 1, Some((2, 1 << 5))), 7);
            assert_eq!(load(p, 2, None), 7);
        }
    }
}
