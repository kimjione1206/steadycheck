//! RAM 검사: 여러 일꾼이 버퍼를 나눠 맡아 기본 세트를 되풀이한다 — A 주소고유값 → B March C- → C 줄무늬 배경 9개.
//! 원소(조각 전체를 한 방향으로 한 번 훑기)가 끝날 때마다 쓰기를 메모리에 밀어 넣고(울타리) 모든 일꾼이 기다린다.
//! 내림차순 원소는 자기 조각을 정확히 거꾸로 돈다 — 가해 칸이 피해 칸 앞에 있든 뒤에 있든 결합 고장이 드러나게.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Barrier;
use std::time::{Duration, Instant};

/// 일부러 넣는 비트 하나. pass = 단계 순번(회차 × 11 + 단계 번호: 0 = A, 1 = B, 2..=10 = C0..C8)
#[derive(Clone, Copy, Debug)]
pub struct MemInject {
    pub pass: u64,
    pub word: usize,
    pub bit: u32,
    /// false 면 단계 첫 원소(쓰기) 직후, true 면 단계 마지막 원소(읽기) 직전에 넣는다
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
    /// 단계 순번 (MemInject.pass 와 같은 셈)
    pub pass: u64,
    /// 단계 이름 "A", "B", "C0".."C8"
    pub stage: &'static str,
    /// 단계 안 원소 번호 (0 부터)
    pub element: usize,
    /// 틀린 칸을 읽은 조작의 배경 이름
    pub pattern: String,
    pub offset_bytes: usize,
    pub expected: String,
    pub actual: String,
    pub reread: String,
    /// 단계 시작 시각
    pub pass_start_ms: u64,
    pub at_ms: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct MemOutcome {
    pub bytes: usize,
    pub threads: usize,
    pub pinned: bool,
    /// 일꾼별로 끝낸 단계 수의 합
    pub passes: u64,
    pub min_thread_passes: u64,
    /// 읽어서 대조한 바이트 (읽기 한 번 = 8바이트)
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

/// 원소가 칸을 도는 방향. Any 는 Up 으로 돈다
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Order {
    Up,
    Down,
    Any,
}

/// 배경: 칸 번호(버퍼 전체 기준)마다 쓸 값
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bg {
    /// 칸마다 다른 값 splitmix64(칸)
    Hash,
    /// 전부 0
    Solid,
    /// 캐시 줄 줄무늬 k (0..=8): 줄 안 위치 q = 64·(칸 % 8) + 비트 의 비트 값 = (q >> k) & 1
    Stripe(u32),
    /// 씨앗 s 의 무작위 splitmix64(s ^ 칸)
    Random(u64),
}

/// 칸 하나에 하는 조작: 배경 값(true 면 뒤집은 값)을 쓰거나 읽어 대조
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    W(Bg, bool),
    R(Bg, bool),
}

/// 원소: 한 방향으로 칸을 차례로 돌며 칸마다 ops 를 순서대로 한다
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub order: Order,
    pub ops: Vec<Op>,
}

impl Element {
    fn reads(&self) -> u64 {
        self.ops.iter().filter(|op| matches!(op, Op::R(..))).count() as u64
    }
}

/// 한 회차에 도는 단계 수 (A, B, C0..C8)
pub const STAGES: u64 = 11;

const C_NAMES: [&str; 9] = ["C0", "C1", "C2", "C3", "C4", "C5", "C6", "C7", "C8"];

/// 기본 세트: A 주소고유값 ⇑(w h);⇑(r h) — 2n, B March C- ⇕(w0);⇑(r0,w1);⇑(r1,w0);⇓(r0,w1);⇓(r1,w0);⇕(r0) — 10n,
/// C0..C8 줄무늬 b 마다 ⇕(w b);⇑(r b,w b̄);⇓(r b̄,w b);⇕(r b) — 6n × 9. 합 66n
pub fn base_set() -> Vec<(&'static str, Vec<Element>)> {
    use Op::{R, W};
    use Order::{Any, Down, Up};
    let el = |order, ops: &[Op]| Element { order, ops: ops.to_vec() };
    let (h, z) = (Bg::Hash, Bg::Solid);
    let mut out = vec![
        ("A", vec![el(Up, &[W(h, false)]), el(Up, &[R(h, false)])]),
        (
            "B",
            vec![
                el(Any, &[W(z, false)]),
                el(Up, &[R(z, false), W(z, true)]),
                el(Up, &[R(z, true), W(z, false)]),
                el(Down, &[R(z, false), W(z, true)]),
                el(Down, &[R(z, true), W(z, false)]),
                el(Any, &[R(z, false)]),
            ],
        ),
    ];
    for (k, name) in C_NAMES.into_iter().enumerate() {
        let b = Bg::Stripe(k as u32);
        out.push((name, vec![el(Any, &[W(b, false)]), el(Up, &[R(b, false), W(b, true)]), el(Down, &[R(b, true), W(b, false)]), el(Any, &[R(b, false)])]));
    }
    out
}

/// 줄무늬 k < 6 은 칸 안 비트 번호 j 의 (j >> k) & 1 — 칸마다 같다
const STRIPE: [u64; 6] = [0xAAAA_AAAA_AAAA_AAAA, 0xCCCC_CCCC_CCCC_CCCC, 0xF0F0_F0F0_F0F0_F0F0, 0xFF00_FF00_FF00_FF00, 0xFFFF_0000_FFFF_0000, 0xFFFF_FFFF_0000_0000];

/// 배경 bg 의 칸 i(버퍼 전체 번호) 값
#[inline(always)]
fn value(bg: Bg, i: usize) -> u64 {
    match bg {
        Bg::Hash => crate::kernel::splitmix64(i as u64),
        Bg::Solid => 0,
        Bg::Stripe(k) if k < 6 => STRIPE[k as usize],
        // k = 6..8: 줄 안 칸 번호(0..7)의 (k - 6) 번째 비트로 칸 전체가 0 또는 1
        Bg::Stripe(k) => 0u64.wrapping_sub((i as u64 % 8) >> (k - 6) & 1),
        Bg::Random(s) => crate::kernel::splitmix64(s ^ i as u64),
    }
}

fn bg_name(bg: Bg) -> String {
    match bg {
        Bg::Hash => "hash".into(),
        Bg::Solid => "solid".into(),
        Bg::Stripe(k) => format!("stripe {k}"),
        Bg::Random(_) => "random".into(),
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
    put(ptr.add(i), v);
}

/// 캐시를 거치지 않는 저장(x86) — 쓴 값이 캐시에 머물다 늦게 도착해 원소 순서가 흐트러지지 않게
#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn put(p: *mut u64, v: u64) {
    std::arch::x86_64::_mm_stream_si64(p as *mut i64, v as i64);
}

#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
unsafe fn put(p: *mut u64, v: u64) {
    p.write_volatile(v);
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

/// 검사 코드가 메모리를 만지는 유일한 창구. 실제 실행은 버퍼, 시험은 고장 모형(memsim)을 끼운다
pub trait Cells {
    fn len(&self) -> usize;
    fn read(&mut self, i: usize) -> u64;
    fn write(&mut self, i: usize, v: u64);
    /// 앞서 쓴 값이 메모리에 닿도록 기다린다
    fn fence(&mut self) {}
}

/// 실제 버퍼 창구: 일꾼 조각의 앞 len 칸. 고장 흉내 갈고리(couple·busy)는 여기서 걸린다.
/// 읽기는 volatile(컴파일러가 "쓴 값을 그대로 안다"며 읽기를 생략하지 못하게), 쓰기는 캐시 우회 저장
struct Buf {
    ptr: *mut u64,
    len: usize,
    couple: Option<(usize, usize, u64)>,
    busy: Option<(usize, u64)>,
}

impl Cells for Buf {
    fn len(&self) -> usize {
        self.len
    }
    // 안전: Buf 는 worker 가 자기 조각으로만 만들고, 실행기는 0..len 칸만 만진다
    #[inline(always)]
    fn read(&mut self, i: usize) -> u64 {
        debug_assert!(i < self.len);
        unsafe { load(self.ptr, i, self.busy) }
    }
    #[inline(always)]
    fn write(&mut self, i: usize, v: u64) {
        debug_assert!(i < self.len);
        unsafe { store(self.ptr, self.len, i, v, self.couple) }
    }
    fn fence(&mut self) {
        // 캐시 우회 저장은 순서가 느슨하다 — 원소 끝에서 모두 메모리에 닿게 한다
        #[cfg(target_arch = "x86_64")]
        unsafe {
            std::arch::x86_64::_mm_sfence()
        }
    }
}

/// 원소의 k 번째 차례가 만지는 칸 (칸 n 개)
#[inline(always)]
pub fn slot(order: Order, n: usize, k: usize) -> usize {
    if order == Order::Down { n - 1 - k } else { k }
}

/// 칸 i 에 원소의 조작을 차례로 한다. base = c 의 칸 0 의 버퍼 전체 번호. 읽어 틀리면 Err((기대값, 읽은 값, 배경))
#[inline(always)]
pub fn step<C: Cells + ?Sized>(c: &mut C, base: usize, el: &Element, i: usize) -> Result<(), (u64, u64, Bg)> {
    for &op in &el.ops {
        match op {
            Op::W(bg, inv) => c.write(i, value(bg, base + i) ^ 0u64.wrapping_sub(inv as u64)),
            Op::R(bg, inv) => {
                let want = value(bg, base + i) ^ 0u64.wrapping_sub(inv as u64);
                let got = c.read(i);
                if got != want {
                    return Err((want, got, bg));
                }
            }
        }
    }
    Ok(())
}

/// 원소 하나에서 처음 어긋난 곳
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Miss {
    /// 틀린 칸
    pub i: usize,
    /// 그 전에 다 끝낸 칸 수
    pub done: usize,
    pub want: u64,
    pub got: u64,
    pub bg: Bg,
}

/// 원소 하나로 c 의 칸 전체를 훑는다. 기본 세트의 세 모양((w), (r), 같은 배경의 (r, w))은 배경별로 따로 만든 빠른 루프로,
/// 그 밖의 모양은 step 으로 돈다 — 두 길은 결과가 같아야 한다(시험 run_element_matches_step)
pub fn run_element<C: Cells + ?Sized>(c: &mut C, base: usize, el: &Element) -> Result<(), Miss> {
    let shape = match el.ops[..] {
        [Op::W(bg, inv)] => Some((bg, Shape::W(inv))),
        [Op::R(bg, inv)] => Some((bg, Shape::R(inv))),
        [Op::R(bg, ri), Op::W(wb, wi)] if bg == wb => Some((bg, Shape::Rw(ri, wi))),
        _ => None,
    };
    let Some((bg, shape)) = shape else {
        let n = c.len();
        for k in 0..n {
            let i = slot(el.order, n, k);
            step(c, base, el, i).map_err(|(want, got, bg)| Miss { i, done: k, want, got, bg })?;
        }
        return Ok(());
    };
    // 배경마다 값 함수를 따로 넣어 루프 안에서 배경을 고르지 않게 한다
    let r = match bg {
        Bg::Hash => sweep(c, base, el.order, shape, |i| crate::kernel::splitmix64(i as u64)),
        Bg::Solid => sweep(c, base, el.order, shape, |_| 0),
        Bg::Stripe(k) if k < 6 => {
            let v = STRIPE[k as usize];
            sweep(c, base, el.order, shape, move |_| v)
        }
        Bg::Stripe(k) => sweep(c, base, el.order, shape, move |i| 0u64.wrapping_sub((i as u64 % 8) >> (k - 6) & 1)),
        Bg::Random(s) => sweep(c, base, el.order, shape, move |i| crate::kernel::splitmix64(s ^ i as u64)),
    };
    r.map_err(|(i, done, want, got)| Miss { i, done, want, got, bg })
}

/// 원소 모양: 쓰기만 / 읽기만 / 읽고 쓰기 (각각 뒤집기 여부)
#[derive(Clone, Copy)]
enum Shape {
    W(bool),
    R(bool),
    Rw(bool, bool),
}

/// 한 원소의 빠른 루프. f = 배경 값(버퍼 전체 칸 번호 → 값). 틀리면 Err((칸, 끝낸 칸 수, 기대값, 읽은 값))
#[inline(always)]
fn sweep<C: Cells + ?Sized, F: Fn(usize) -> u64>(c: &mut C, base: usize, order: Order, shape: Shape, f: F) -> Result<(), (usize, usize, u64, u64)> {
    let n = c.len();
    let inv = |x: bool| 0u64.wrapping_sub(x as u64);
    match shape {
        Shape::W(w) => {
            let w = inv(w);
            for k in 0..n {
                let i = slot(order, n, k);
                c.write(i, f(base + i) ^ w);
            }
        }
        Shape::R(r) => {
            let r = inv(r);
            for k in 0..n {
                let i = slot(order, n, k);
                let want = f(base + i) ^ r;
                let got = c.read(i);
                if got != want {
                    return Err((i, k, want, got));
                }
            }
        }
        Shape::Rw(r, w) => {
            let (r, w) = (inv(r), inv(w));
            for k in 0..n {
                let i = slot(order, n, k);
                let v = f(base + i);
                let got = c.read(i);
                if got != v ^ r {
                    return Err((i, k, v ^ r, got));
                }
                c.write(i, v ^ w);
            }
        }
    }
    Ok(())
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
    let barrier = Barrier::new(threads);
    let outs: Vec<WorkerOut> = std::thread::scope(|s| {
        let mut rest: &mut [u64] = &mut buf;
        let mut handles = Vec::new();
        for t in 0..threads {
            let len = if t + 1 == threads { rest.len() } else { starts[t + 1] - starts[t] };
            let (chunk, tail) = rest.split_at_mut(len);
            rest = tail;
            let (base, stop, barrier) = (starts[t], &stop, &barrier);
            handles.push(s.spawn(move || worker(cfg, start, stop, barrier, t, threads, base, chunk)));
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

/// 일꾼 t: 버퍼 전체의 칸 base.. 에 해당하는 자기 조각에 기본 세트를 되풀이한다.
/// 멈춤(오류·마감)은 원소 시작마다 모든 일꾼이 같은 자리에서 함께 판단한다 — 대기 두 번 사이에서 깃발을 읽으므로
/// 누구는 멈추고 누구는 다음 대기에서 영영 기다리는 일이 없다 (보고서의 설계 설명 참고)
#[allow(clippy::too_many_arguments)]
fn worker(cfg: &MemConfig, start: Instant, stop: &AtomicBool, barrier: &Barrier, t: usize, threads: usize, base: usize, chunk: &mut [u64]) -> WorkerOut {
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
    let flip = |at: Option<(usize, u32)>| {
        if let Some((w, bit)) = at {
            unsafe {
                let q = ptr.add(w);
                q.write_volatile(q.read_volatile() ^ (1u64 << (bit % 64)));
            }
        }
    };
    // 원소 시작마다 모두 함께: 마감이면 깃발을 세우고, 대기 → 깃발 읽기 → 대기. 두 대기 사이에는 아무도 깃발을 바꾸지 않는다
    let halt = || {
        if start.elapsed() >= cfg.duration {
            stop.store(true, Ordering::Relaxed);
        }
        barrier.wait();
        let h = stop.load(Ordering::Relaxed);
        barrier.wait();
        h
    };
    let mut cells = Buf { ptr, len: n, couple, busy };
    let prog = base_set();
    let mut pass = 0u64;
    let mut verified = 0u64;
    'run: loop {
        for (name, els) in &prog {
            let flip_at = |late: bool| cfg.inject.filter(|j| j.pass == pass && j.late == late).and_then(|j| local(j.word).map(|w| (w, j.bit)));
            let mut pass_start_ms = 0;
            for (e, el) in els.iter().enumerate() {
                if halt() {
                    break 'run;
                }
                if e == 0 {
                    pass_start_ms = ms();
                }
                if e + 1 == els.len() {
                    flip(flip_at(true));
                }
                let r = run_element(&mut cells, base, el);
                cells.fence();
                if let Err(m) = r {
                    stop.store(true, Ordering::Relaxed);
                    let error = MemError {
                        thread: t,
                        pass,
                        stage: name,
                        element: e,
                        pattern: bg_name(m.bg),
                        offset_bytes: (base + m.i) * 8,
                        expected: format!("{:#018x}", m.want),
                        actual: format!("{:#018x}", m.got),
                        reread: format!("{:#018x}", unsafe { ptr.add(m.i).read_volatile() }),
                        pass_start_ms,
                        at_ms: ms(),
                    };
                    verified += m.done as u64 * el.reads() * 8;
                    // 다른 일꾼이 다음 원소 시작에서 멈추도록 같은 자리의 대기에 한 번 더 들어간다
                    halt();
                    return WorkerOut { pinned, passes: pass + 1, verified, error: Some(error) };
                }
                verified += n as u64 * el.reads() * 8;
                if e == 0 {
                    flip(flip_at(false));
                }
            }
            pass += 1;
        }
    }
    WorkerOut { pinned, passes: pass, verified, error: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const MB8: usize = 8 * 1024 * 1024;
    const N8: u64 = (MB8 / 8) as u64;

    fn run1(mb: usize, secs: u64, threads: usize, inject: Option<MemInject>) -> MemOutcome {
        run(&MemConfig { mb, duration: Duration::from_secs(secs), threads, inject, fault: None })
    }

    #[test]
    fn clean_run_has_no_error() {
        let out = run(&MemConfig { mb: 1, duration: Duration::from_millis(500), threads: 1, inject: None, fault: None });
        assert!(out.error.is_none(), "{:?}", out.error);
        assert!(out.passes >= STAGES, "기본 세트 한 회차는 끝내야 한다: {}", out.passes);
        // 한 회차 = 읽기 33n
        assert!(out.bytes_verified >= 33 * (1 << 20));
    }

    #[test]
    fn base_set_shape() {
        let set = base_set();
        let names: Vec<_> = set.iter().map(|s| s.0).collect();
        assert_eq!(names, ["A", "B", "C0", "C1", "C2", "C3", "C4", "C5", "C6", "C7", "C8"]);
        let ops: usize = set.iter().flat_map(|s| &s.1).map(|e| e.ops.len()).sum();
        let reads: u64 = set.iter().flat_map(|s| &s.1).map(|e| e.reads()).sum();
        assert_eq!((ops, reads), (66, 33));
        // 모든 단계는 쓰기만 하는 원소로 시작해 읽기만 하는 원소로 끝난다 (주입 자리)
        for (name, els) in &set {
            assert!(els[0].ops.iter().all(|o| matches!(o, Op::W(..))), "{name}");
            assert!(els.last().unwrap().ops.iter().all(|o| matches!(o, Op::R(..))), "{name}");
        }
        // March C- 는 오름 2·내림 2
        let b = &set[1].1;
        assert_eq!(b.iter().map(|e| e.order).collect::<Vec<_>>(), [Order::Any, Order::Up, Order::Up, Order::Down, Order::Down, Order::Any]);
        assert_eq!(set[5].1[2], Element { order: Order::Down, ops: vec![Op::R(Bg::Stripe(3), true), Op::W(Bg::Stripe(3), false)] });
    }

    #[test]
    fn stripe_bits_follow_line_position() {
        for k in 0..9u32 {
            for i in 0..16usize {
                let v = value(Bg::Stripe(k), i);
                for j in 0..64 {
                    let q = 64 * (i % 8) + j;
                    assert_eq!(v >> j & 1, (q as u64 >> k) & 1, "k={k} i={i} j={j}");
                }
            }
        }
        // 줄 안 서로 다른 두 위치는 어떤 줄무늬에서 반드시 값이 갈린다
        for q1 in 0..512usize {
            for q2 in q1 + 1..512 {
                let bit = |k, q: usize| value(Bg::Stripe(k), q / 64) >> (q % 64) & 1;
                assert!((0..9).any(|k| bit(k, q1) != bit(k, q2)), "{q1} {q2}");
            }
        }
    }

    #[test]
    fn other_backgrounds_are_known() {
        assert_eq!(value(Bg::Solid, 77), 0);
        assert_eq!(value(Bg::Hash, 5), crate::kernel::splitmix64(5));
        assert_eq!(value(Bg::Random(9), 5), crate::kernel::splitmix64(9 ^ 5));
        assert_ne!(value(Bg::Hash, 5), value(Bg::Hash, 6));
        assert_eq!((bg_name(Bg::Hash), bg_name(Bg::Solid), bg_name(Bg::Stripe(7)), bg_name(Bg::Random(1))), ("hash".into(), "solid".into(), "stripe 7".into(), "random".into()));
    }

    #[test]
    fn down_visits_exact_reverse() {
        assert_eq!((0..5).map(|k| slot(Order::Down, 5, k)).collect::<Vec<_>>(), [4, 3, 2, 1, 0]);
        assert_eq!((0..5).map(|k| slot(Order::Up, 5, k)).collect::<Vec<_>>(), [0, 1, 2, 3, 4]);
        assert_eq!((0..5).map(|k| slot(Order::Any, 5, k)).collect::<Vec<_>>(), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn injected_flip_is_caught_at_exact_place() {
        let inj = MemInject { pass: 2, word: 12_345, bit: 17, late: false };
        let out = run1(8, 5, 1, Some(inj));
        let e = out.error.clone().expect("주입한 오류를 잡아야 한다");
        assert_eq!((e.pass, e.stage, e.element, e.offset_bytes), (2, "C0", 1, 12_345 * 8));
        assert_ne!(e.expected, e.actual);
        assert_eq!(e.actual, e.reread, "뒤집힌 값은 다시 읽어도 같아야 한다");
        assert!(out.failed());
    }

    // 오류 보고의 숫자가 정확해야 한다 (단계 3 = C1, 줄무늬 1 = 0xCCCC…)
    #[test]
    fn error_report_numbers_are_exact() {
        let inj = MemInject { pass: 3, word: 12_345, bit: 17, late: false };
        let out = run1(8, 5, 1, Some(inj));
        assert_eq!((out.bytes, out.passes), (MB8, 4));
        // A 읽기 1n, B 5n, C0 3n, 그리고 C1 원소 1 에서 12,345칸
        assert_eq!(out.bytes_verified, (1 + 5 + 3) * N8 * 8 + 12_345 * 8);
        let e = out.error.expect("주입한 오류를 잡아야 한다");
        assert_eq!((e.stage, e.element, e.pattern.as_str()), ("C1", 1, "stripe 1"));
        assert_eq!(e.expected, format!("{:#018x}", 0xCCCC_CCCC_CCCC_CCCCu64));
        assert_eq!(e.actual, format!("{:#018x}", 0xCCCC_CCCC_CCCC_CCCCu64 ^ (1 << 17)));
        assert!(e.at_ms >= e.pass_start_ms);
    }

    #[test]
    fn late_flip_is_caught_in_last_element() {
        // 마지막 원소(읽기) 직전에 넣으면 그 원소에서 잡힌다: B 의 마지막은 ⇕(r0)
        let inj = MemInject { pass: 1, word: 12_345, bit: 17, late: true };
        let out = run1(8, 5, 1, Some(inj));
        let e = out.error.expect("늦은 주입을 잡아야 한다");
        assert_eq!((e.pass, e.stage, e.element, e.offset_bytes), (1, "B", 5, 12_345 * 8));
        assert_eq!((e.expected.as_str(), e.actual), ("0x0000000000000000", format!("{:#018x}", 1u64 << 17)));
        // A 1n + B 앞 원소들 4n + 마지막 원소 12,345칸
        assert_eq!((out.passes, out.bytes_verified), (2, 5 * N8 * 8 + 12_345 * 8));
    }

    #[test]
    fn injection_in_later_round_is_caught() {
        // 두 번째 회차의 A = 단계 순번 11
        let inj = MemInject { pass: STAGES, word: 7, bit: 0, late: false };
        let e = run1(1, 10, 1, Some(inj)).error.expect("두 번째 회차 주입을 잡아야 한다");
        assert_eq!((e.pass, e.stage, e.offset_bytes), (STAGES, "A", 56));
    }

    // 버퍼 밖 위치의 주입은 무시한다 (버퍼 밖에 쓰면 안 된다)
    #[test]
    fn out_of_range_injection_is_ignored() {
        let inj = MemInject { pass: 0, word: MB8 / 8, bit: 0, late: false };
        let out = run(&MemConfig { mb: 8, duration: Duration::from_millis(300), threads: 1, inject: Some(inj), fault: None });
        assert!(out.error.is_none(), "{:?}", out.error);
    }

    #[test]
    fn coupling_spreads_only_on_rising_bit() {
        // 버퍼는 앞 4칸만 쓰고, 그 뒤 칸은 버퍼 밖에 써 버리는지 지켜본다
        let mut b = [0u64; 12];
        let p = b.as_mut_ptr();
        let c = Some((1, 2, 1u64 << 3));
        unsafe {
            store(p, 4, 1, 0b0001, c); // 비트 3 은 그대로 0 → 번지지 않음
            assert_eq!(p.add(3).read_volatile(), 0);
            store(p, 4, 1, 0b1001, c); // 비트 3 이 0 → 1 → 칸 3 에 번짐
            assert_eq!((p.add(1).read_volatile(), p.add(3).read_volatile()), (0b1001, 0b1000));
            p.add(3).write(0);
            store(p, 4, 1, 0b1001, c); // 이미 1 → 1: 번지지 않음
            assert_eq!(p.add(3).read_volatile(), 0);
            store(p, 4, 2, 0b1000, c); // 가해 칸이 아니면 번지지 않음
            assert_eq!(p.add(3).read_volatile(), 0);
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
        // 원소마다 함께 멈추므로 일꾼마다 끝낸 단계 수가 같다
        assert_eq!(out.passes, 4 * out.min_thread_passes);
        assert_eq!(out.bytes_verified % (2 << 20), 0, "조각(2MB) 단위로 셈");
        assert!(out.verified_bytes_per_sec > 0);
    }

    #[test]
    fn error_in_last_chunk_names_its_worker() {
        // 8MB = 1,048,576 칸, 일꾼 3명 → 시작 0 / 349,525 / 699,050, 마지막 칸은 일꾼 2
        let words = MB8 / 8;
        let inj = MemInject { pass: 1, word: words - 1, bit: 63, late: false };
        let e = run1(8, 5, 3, Some(inj)).error.expect("마지막 칸 주입을 잡아야 한다");
        assert_eq!((e.thread, e.pass, e.offset_bytes), (2, 1, (words - 1) * 8));
    }

    #[test]
    fn error_in_one_worker_stops_run_as_fail() {
        // 64MB, 일꾼 2명 → 일꾼 1 조각의 첫 칸에 단계 0 주입. 다른 일꾼은 함께 멈추고 결과는 FAIL
        let words = 64 * 1024 * 1024 / 8;
        let inj = MemInject { pass: 0, word: words / 2, bit: 5, late: false };
        let out = run1(64, 10, 2, Some(inj));
        assert!(out.failed());
        let e = out.error.expect("일꾼 1 조각의 주입을 잡아야 한다");
        assert_eq!((e.thread, e.pass, e.offset_bytes), (1, 0, words / 2 * 8));
        // 일꾼 0 은 오류가 난 다음 원소 시작에서 멈춘다: A 를 끝냈거나(1) 못 끝냈다(0) — 오류 일꾼은 1
        assert!(out.passes <= 2, "{}", out.passes);
    }

    #[test]
    fn deadline_stops_all_workers_together() {
        // 큰 버퍼·짧은 마감: 기본 세트 중간에 멈춰도 일꾼끼리 기다리다 멈추지 않는다
        let t = Instant::now();
        let out = run(&MemConfig { mb: 256, duration: Duration::from_millis(200), threads: 3, inject: None, fault: None });
        assert!(out.error.is_none());
        assert!(t.elapsed() < Duration::from_secs(20));
        assert_eq!(out.passes % 3, 0, "일꾼마다 같은 수의 단계");
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
