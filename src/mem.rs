//! RAM 검사: 여러 일꾼이 버퍼를 나눠 맡아 먼저 기본 세트(A 주소고유값 → B March C- → C 줄무늬 배경 9개)를 한 번 돌고,
//! 남은 시간은 D(무작위 배경 March C-, 회차마다 주소 순서·조각 맡기를 바꿈)와 E(버스 스트레스)를 시간 6:4 로 번갈아 돈다.
//! 원소(조각 전체를 한 방향으로 한 번 훑기)가 끝날 때마다 쓰기를 메모리에 밀어 넣고(울타리) 모든 일꾼이 기다린다.
//! 내림차순 원소는 자기 조각을 정확히 거꾸로 돈다 — 같은 조각 안의 쌍은 가해 칸이 앞에 있든 뒤에 있든 결합 고장이 드러난다.
//! ⇓ 는 조각 안에서만 순서를 뒤집으므로 서로 다른 조각의 쌍은 속도와 상관없이 보장 밖이다(속도 차이가 나면 놓치는 쌍이 늘어난다).
//! D 는 홀수 회차마다 조각을 거꾸로 맡아 그런 쌍에 반대 방문 순서를 준다(시뮬레이터 시험 참고).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::kernel::splitmix64;
use std::sync::Barrier;
use std::time::{Duration, Instant};

/// 일부러 넣는 비트 하나. pass = 단계 순번(0 = A, 1 = B, 2..=10 = C0..C8, 11 = 첫 D, 12 = 첫 E, 그 뒤는 시간에 따라 D/E)
#[derive(Clone, Copy, Debug)]
pub struct MemInject {
    pub pass: u64,
    pub word: usize,
    pub bit: u32,
    /// false 면 단계 첫 원소(쓰기) 직후, true 면 단계 마지막 원소(읽기) 직전에 넣는다.
    /// E 에서는 late 와 상관없이 그 칸이 든 64KiB 묶음을 쓴 직후에 넣는다. E 는 조각을 줄 단위 절반 둘로 나누므로
    /// 줄 수가 홀수인 조각의 마지막 줄은 E 가 쓰지 않고, 그 칸을 겨냥한 E 주입은 무시된다(그 칸은 D 가 검사한다)
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
    /// 단계 이름 "A", "B", "C0".."C8", "D", "E"
    pub stage: &'static str,
    /// 단계 안 원소 번호 (0 부터)
    pub element: usize,
    /// 틀린 칸을 읽은 조작의 배경 이름 (일꾼이 패닉하면 "panic: 메시지", 이때 칸·값 칸은 비어 있다)
    pub pattern: String,
    pub offset_bytes: usize,
    pub expected: String,
    pub actual: String,
    pub reread: String,
    /// 진단 참고(판정과 무관): 다시 읽은 값이 기대값과 같으면 "read"(읽는 길 — 버스·메모리 제어기 쪽),
    /// 칸에 틀린 값이 남아 있으면 "stored", 일꾼 패닉이면 "panic"
    pub kind: &'static str,
    /// 틀린 비트들의 캐시 줄 안 위치 q = 64·(칸 % 8) + 비트 (작은 것부터)
    pub line_bits: Vec<u32>,
    /// 단계 시작 시각
    pub pass_start_ms: u64,
    pub at_ms: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct MemOutcome {
    pub bytes: usize,
    pub threads: usize,
    pub pinned: bool,
    /// 일꾼별로 끝낸 단계 수의 합 (기본 세트 11 + D 회차 + E 바퀴)
    pub passes: u64,
    pub min_thread_passes: u64,
    /// 읽어서 대조한 바이트 (읽기 한 번 = 8바이트)
    pub bytes_verified: u64,
    pub verified_bytes_per_sec: u64,
    pub elapsed_ms: u64,
    /// 모든 일꾼이 기본 세트를 끝냈는지 — false 면 결합 고장 보장이 성립하지 않는다(판정은 그대로, 경고)
    pub base_complete: bool,
    /// 기본 세트에 걸린 초(끝냈으면 실제, 못 끝냈으면 진행률로 늘려 잡은 예상). 한 원소도 못 끝냈으면 없음
    pub base_seconds_estimate: Option<f64>,
    /// 끝낸 D 회차 수 (일꾼 중 최소)
    pub rounds_d: u64,
    /// E 묶음(64KiB 쓰기) 수 (일꾼 합)
    pub bursts_e: u64,
    pub error: Option<MemError>,
    /// 잡은 오류 수(일꾼 합). 기본은 첫 오류에서 멈춰 0 또는 1, --keep-going N 이면 N 안팎 — 일꾼마다 단계 하나에 많아야 하나
    pub errors_total: u64,
    /// --keep-going 일 때 잡은 오류(시각 순, 앞 32개). 기본 실행에서는 비어 JSON 에서 빠진다
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<MemError>,
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
    /// 이만큼 오류를 잡으면 멈춘다 (1 = 첫 오류에서 멈춤, 기본). --keep-going N
    pub max_errors: u64,
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

/// 원소가 줄을 늘어놓는 방식: 차례대로, 또는 쪽 보폭 — 4KiB 쪽(64줄) 안은 차례대로, 쪽 순서는 256KiB(64쪽) 블록마다 같은 자리 쪽을 돌고 다음 자리로
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Walk {
    Linear,
    Stride,
}

/// 원소: 줄을 walk 로 늘어놓은 차례(⇓ 는 정확한 역순)대로 돌며 칸마다 ops 를 한다
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub order: Order,
    pub walk: Walk,
    pub ops: Vec<Op>,
}

impl Element {
    fn reads(&self) -> u64 {
        self.ops.iter().filter(|op| matches!(op, Op::R(..))).count() as u64
    }
}

/// 기본 세트의 단계 수 (A, B, C0..C8)
pub const STAGES: u64 = 11;

/// 기본 세트의 칸당 조작 수
pub const BASE_OPS: u64 = 66;

const C_NAMES: [&str; 9] = ["C0", "C1", "C2", "C3", "C4", "C5", "C6", "C7", "C8"];

/// March C- ⇕(w b);⇑(r b,w b̄);⇑(r b̄,w b);⇓(r b,w b̄);⇓(r b̄,w b);⇕(r b)
fn march_c(b: Bg, walk: Walk) -> Vec<Element> {
    use Op::{R, W};
    use Order::{Any, Down, Up};
    let el = |order, ops: &[Op]| Element { order, walk, ops: ops.to_vec() };
    vec![
        el(Any, &[W(b, false)]),
        el(Up, &[R(b, false), W(b, true)]),
        el(Up, &[R(b, true), W(b, false)]),
        el(Down, &[R(b, false), W(b, true)]),
        el(Down, &[R(b, true), W(b, false)]),
        el(Any, &[R(b, false)]),
    ]
}

/// 기본 세트: A 주소고유값 ⇑(w h);⇑(r h) — 2n, B March C-(배경 0) — 10n,
/// C0..C8 줄무늬 b 마다 ⇕(w b);⇑(r b,w b̄);⇓(r b̄,w b);⇕(r b) — 6n × 9. 합 66n. 모두 선형·제 조각
pub fn base_set() -> Vec<(&'static str, Vec<Element>)> {
    use Op::{R, W};
    use Order::{Any, Down, Up};
    let el = |order, ops: &[Op]| Element { order, walk: Walk::Linear, ops: ops.to_vec() };
    let h = Bg::Hash;
    let mut out = vec![("A", vec![el(Up, &[W(h, false)]), el(Up, &[R(h, false)])]), ("B", march_c(Bg::Solid, Walk::Linear))];
    for (k, name) in C_NAMES.into_iter().enumerate() {
        let b = Bg::Stripe(k as u32);
        out.push((name, vec![el(Any, &[W(b, false)]), el(Up, &[R(b, false), W(b, true)]), el(Down, &[R(b, true), W(b, false)]), el(Any, &[R(b, false)])]));
    }
    out
}

/// D 회차 r(0부터): 무작위 배경 March C-. 회차 두 개(2k, 2k+1)가 같은 배경(씨앗 splitmix64(k))을 쓰고,
/// 짝수 회차는 선형, 홀수 회차는 쪽 보폭(4KiB 쪽 안은 차례대로, 쪽끼리는 256KiB 간격)으로 돈다. 조각 맡기는 chunk_of 참고
pub fn d_round(r: u64) -> Vec<Element> {
    march_c(Bg::Random(splitmix64(r / 2)), if r % 2 == 1 { Walk::Stride } else { Walk::Linear })
}

/// 일꾼 t 가 맡는 조각 번호. 기본 세트·E(d = None)는 제 조각.
/// D 회차 r 은 회차 쌍 k = r / 2 마다 k 칸 돌려 맡고, 홀수 회차는 그것을 거꾸로 맡는다 —
/// ⇓ 는 조각 안에서만 순서를 뒤집으므로, 같은 배경에서 조각 사이 쌍의 방문 순서를 바꿔 주고 일꾼 속도 차이도 조각마다 돌려 준다
pub fn chunk_of(t: usize, workers: usize, d: Option<u64>) -> usize {
    match d {
        None => t,
        Some(r) => {
            let c = (t + (r / 2) as usize) % workers;
            if r % 2 == 1 { workers - 1 - c } else { c }
        }
    }
}

/// 줄무늬 k < 6 은 칸 안 비트 번호 j 의 (j >> k) & 1 — 칸마다 같다
const STRIPE: [u64; 6] = [0xAAAA_AAAA_AAAA_AAAA, 0xCCCC_CCCC_CCCC_CCCC, 0xF0F0_F0F0_F0F0_F0F0, 0xFF00_FF00_FF00_FF00, 0xFFFF_0000_FFFF_0000, 0xFFFF_FFFF_0000_0000];

/// 배경 bg 의 칸 i(버퍼 전체 번호) 값
#[inline(always)]
fn value(bg: Bg, i: usize) -> u64 {
    match bg {
        Bg::Hash => splitmix64(i as u64),
        Bg::Solid => 0,
        Bg::Stripe(k) if k < 6 => STRIPE[k as usize],
        // k = 6..8: 줄 안 칸 번호(0..7)의 (k - 6) 번째 비트로 칸 전체가 0 또는 1
        Bg::Stripe(k) => 0u64.wrapping_sub((i as u64 % 8) >> (k - 6) & 1),
        Bg::Random(s) => splitmix64(s ^ i as u64),
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
    /// 같은 캐시 줄의 칸 i, i+1, … 에 v 를 한꺼번에 쓴다 — 실제 메모리는 줄째 오간다(캐시 우회 저장도 줄째 모아 보낸다).
    /// 기본은 차례로 write
    fn write_line(&mut self, i: usize, v: &[u64]) {
        for (j, &x) in v.iter().enumerate() {
            self.write(i + j, x);
        }
    }
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

/// 캐시 줄 하나의 칸 수 (64비트 × 8 = 512비트)
pub const LINE_WORDS: usize = 8;

/// 고장 흉내 갈고리가 없는 실제 버퍼 창구 (보통 실행) — 칸마다 갈고리를 확인하지 않아 빠르다
struct Plain {
    ptr: *mut u64,
    len: usize,
}

impl Cells for Plain {
    fn len(&self) -> usize {
        self.len
    }
    // 안전: Buf 와 같다 (worker 가 맡은 조각, 실행기는 0..len 만)
    #[inline(always)]
    fn read(&mut self, i: usize) -> u64 {
        debug_assert!(i < self.len);
        unsafe { self.ptr.add(i).read_volatile() }
    }
    #[inline(always)]
    fn write(&mut self, i: usize, v: u64) {
        debug_assert!(i < self.len);
        unsafe { put(self.ptr.add(i), v) }
    }
}

/// 보폭 회차의 쪽 = 4KiB = 64줄
const PAGE_LINES: usize = 64;
/// 보폭 회차의 쪽 보폭 = 64쪽 = 256KiB
const STEP_PAGES: usize = 64;

/// 원소가 도는 줄 차례: 줄 nl 개를 walk 로 늘어놓은 순서, ⇓ 면 그 정확한 역순
pub fn lines(order: Order, walk: Walk, nl: usize) -> impl Iterator<Item = usize> {
    let down = order == Order::Down;
    spans(order, walk, nl).flat_map(move |(first, count)| span_lines(first, count, down))
}

/// 원소가 도는 쪽 차례: (첫 줄, 줄 수) 묶음들 — 묶음 안 줄은 차례대로(⇓ 면 거꾸로) 돈다. 선형은 줄 전체가 묶음 하나라,
/// 뜨거운 루프(sweep 등)는 묶음마다 평범한 범위만 돌고 쪽 순서 계산은 쪽마다 한 번뿐이다
pub fn spans(order: Order, walk: Walk, nl: usize) -> impl Iterator<Item = (usize, usize)> {
    let (page, step) = if walk == Walk::Stride { (PAGE_LINES, STEP_PAGES) } else { (nl.max(1), 1) };
    page_spans(order, page, step, nl)
}

/// 묶음 (first, count) 의 줄들: 차례대로, down 이면 거꾸로
#[inline(always)]
fn span_lines(first: usize, count: usize, down: bool) -> impl Iterator<Item = usize> {
    (0..count).map(move |k| if down { first + count - 1 - k } else { first + k })
}

/// 쪽 단위로 늘어놓은 줄 차례: 줄을 page 줄씩 쪽으로 묶고, 쪽 순서를 step 쪽 보폭으로 섞은 뒤
/// (블록(step 쪽)마다 0번째 쪽들을 먼저, 다음 1번째 쪽들…), 쪽 안에서는 줄을 차례대로 돈다. ⇓ 면 정확한 역순(쪽 순서도, 쪽 안 줄도 거꾸로).
/// 줄마다 새 4KiB 쪽으로 뛰면(쪽 1줄·보폭 64) 매 접근이 TLB 미스·미리 읽기 없음·행 바꿈이라 선형보다 몇 배 느리므로, 실제 보폭 회차는 4KiB 쪽 안을 이어 돈다.
/// page = 1 이면 줄 보폭 step 의 순서, page = step = 1 이면 차례대로. 작은 시뮬레이터에서 쪽·보폭 효과를 보려고 둘을 따로 받는다
pub fn lines_by(order: Order, page: usize, step: usize, nl: usize) -> impl Iterator<Item = usize> {
    let down = order == Order::Down;
    page_spans(order, page, step, nl).flat_map(move |(first, count)| span_lines(first, count, down))
}

/// lines_by 의 쪽 차례: (쪽 첫 줄, 쪽 줄 수) — 마지막 쪽은 짧을 수 있다
fn page_spans(order: Order, page: usize, step: usize, nl: usize) -> impl Iterator<Item = (usize, usize)> {
    let pages = nl.div_ceil(page);
    let blocks = pages.div_ceil(step);
    let total = step * blocks;
    let down = order == Order::Down;
    (0..total)
        .map(move |k| if down { total - 1 - k } else { k })
        .map(move |k| (k % blocks) * step + k / blocks)
        .filter(move |&p| p < pages)
        .map(move |p| (p * page, page.min(nl - p * page)))
}

/// 줄 line 의 칸 범위 (시작, 칸 수) — 마지막 줄은 짧을 수 있다
#[inline(always)]
fn line_span(n: usize, line: usize) -> (usize, usize) {
    let lo = line * LINE_WORDS;
    (lo, LINE_WORDS.min(n - lo))
}

/// 원소 하나에서 처음 어긋난 곳
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Miss {
    /// 틀린 칸
    pub i: usize,
    /// 이 원소에서 그 전에 대조를 마친 칸 수
    pub done: usize,
    pub want: u64,
    pub got: u64,
    pub bg: Bg,
}

#[inline(always)]
fn mask(inv: bool) -> u64 {
    0u64.wrapping_sub(inv as u64)
}

/// 원소를 줄 하나에 적용한다: 조작마다 그 줄의 칸들을 차례로(⇓ 면 줄 안에서도 거꾸로) — 읽기는 줄째 대조한 뒤 쓰기는 줄째 쓴다.
/// 실제 메모리도 줄 단위로 오가고, 같은 줄을 칸마다 읽고 캐시 우회 저장으로 쓰기를 번갈아 하면 매번 메모리를 왕복하므로 줄째 묶는다.
/// base = c 의 칸 0 의 버퍼 전체 번호, done = 이 원소에서 앞서 대조를 마친 칸 수.
/// 줄 경계는 c 안의 번호 / 8 로 나누므로, c 의 칸 0 이 실제 캐시 줄의 시작이어야
/// (실제 실행: 64바이트 정렬된 영역 + 8의 배수 base) 이 줄이 실제 줄과 맞는다
pub fn step<C: Cells + ?Sized>(c: &mut C, base: usize, el: &Element, line: usize, done: usize) -> Result<(), Miss> {
    let (lo, m) = line_span(c.len(), line);
    let down = el.order == Order::Down;
    for &op in &el.ops {
        match op {
            Op::W(bg, inv) => {
                let mut v = [0u64; LINE_WORDS];
                for (j, x) in v[..m].iter_mut().enumerate() {
                    *x = value(bg, base + lo + j) ^ mask(inv);
                }
                c.write_line(lo, &v[..m]);
            }
            Op::R(bg, inv) => {
                for j in 0..m {
                    let i = if down { lo + m - 1 - j } else { lo + j };
                    let want = value(bg, base + i) ^ mask(inv);
                    let got = c.read(i);
                    if got != want {
                        return Err(Miss { i, done: done + j, want, got, bg });
                    }
                }
            }
        }
    }
    Ok(())
}

/// 원소 하나로 c 의 칸 전체를 줄씩 훑는다(줄 경계 조건은 step 과 같음 — base 는 8의 배수, c 의 칸 0 은 줄 시작).
/// 기본 세트·D 의 세 모양((w), (r), 같은 배경의 (r, w))은 배경별로 따로 만든 빠른 루프로,
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
        let mut done = 0;
        let down = el.order == Order::Down;
        for (first, count) in spans(el.order, el.walk, n.div_ceil(LINE_WORDS)) {
            for k in 0..count {
                let line = if down { first + count - 1 - k } else { first + k };
                step(c, base, el, line, done)?;
                done += line_span(n, line).1;
            }
        }
        return Ok(());
    };
    // 배경마다 값 함수를 따로 넣어 루프 안에서 배경을 고르지 않게 한다
    let r = match bg {
        Bg::Hash => sweep(c, base, el, shape, |i| splitmix64(i as u64)),
        Bg::Solid => sweep(c, base, el, shape, |_| 0),
        Bg::Stripe(k) if k < 6 => {
            let v = STRIPE[k as usize];
            sweep(c, base, el, shape, move |_| v)
        }
        Bg::Stripe(k) => sweep(c, base, el, shape, move |i| 0u64.wrapping_sub((i as u64 % 8) >> (k - 6) & 1)),
        Bg::Random(s) => sweep(c, base, el, shape, move |i| splitmix64(s ^ i as u64)),
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

/// 한 원소의 빠른 루프(step 과 같은 순서). f = 배경 값(버퍼 전체 칸 번호 → 값). 틀리면 Err((칸, 대조를 마친 칸 수, 기대값, 읽은 값))
#[inline(always)]
fn sweep<C: Cells + ?Sized, F: Fn(usize) -> u64>(c: &mut C, base: usize, el: &Element, shape: Shape, f: F) -> Result<(), (usize, usize, u64, u64)> {
    let n = c.len();
    let down = el.order == Order::Down;
    let mut done = 0;
    for (first, count) in spans(el.order, el.walk, n.div_ceil(LINE_WORDS)) {
        for k in 0..count {
            let line = if down { first + count - 1 - k } else { first + k };
            let (lo, m) = line_span(n, line);
            let (r, w) = match shape {
                Shape::W(w) => (None, Some(mask(w))),
                Shape::R(r) => (Some(mask(r)), None),
                Shape::Rw(r, w) => (Some(mask(r)), Some(mask(w))),
            };
            // 한 줄: 먼저 다 읽어 대조하고, 그다음 줄째 쓴다
            if let Some(r) = r {
                for j in 0..m {
                    let i = if down { lo + m - 1 - j } else { lo + j };
                    let want = f(base + i) ^ r;
                    let got = c.read(i);
                    if got != want {
                        return Err((i, done + j, want, got));
                    }
                }
            }
            if let Some(w) = w {
                let mut v = [0u64; LINE_WORDS];
                for (j, x) in v[..m].iter_mut().enumerate() {
                    *x = f(base + lo + j) ^ w;
                }
                c.write_line(lo, &v[..m]);
            }
            done += m;
        }
    }
    Ok(())
}

/// 버퍼를 일꾼 수만큼 나눈 조각들의 시작 칸 — 캐시 줄(8칸) 경계에 맞춘다. 나머지는 마지막 일꾼이 맡는다
pub fn chunk_starts(words: usize, threads: usize) -> Vec<usize> {
    let per = words / threads / LINE_WORDS * LINE_WORDS;
    (0..threads).map(|t| t * per).collect()
}

/// 주소 addr(8바이트 정렬)에서 다음 64바이트 경계까지 건너뛸 칸 수
fn line_skip(addr: usize) -> usize {
    ((64 - addr % 64) % 64) / 8
}

/// E 의 묶음 크기: 64KiB
const E_BLOCK: usize = 64 * 1024 / 8;

/// E 묶음 씨앗을 D 회차 씨앗과 다른 값으로
const E_SALT: u64 = 0xE5E5_E5E5_E5E5_E5E5;

/// E 묶음 번호 b 의 배경
fn e_bg(b: u64) -> Bg {
    Bg::Random(splitmix64(b ^ E_SALT))
}

/// E 한 바퀴를 시작할 때 조각에 들어 있어야 할 값 — 덮어쓰기 전에 대조하려고
#[derive(Clone, Copy, Debug, PartialEq)]
enum Held {
    /// 조각 전체가 이 배경(true 면 뒤집은 값) — 원소 목록 단계(기본 세트·D)가 마지막으로 쓴 값
    Bg(Bg, bool),
    /// 직전 단계가 같은 일꾼의 E 한 바퀴: 묶음 k 는 묶음 번호 (첫 묶음 + k) 의 배경
    E(u64),
    /// 직전 단계가 오류로 덜 끝나 칸 값을 모른다 — 덮기 전 대조를 건너뛴다
    Unknown,
}

/// 원소 목록 단계가 끝난 뒤 칸에 남는 값: 마지막 쓰기 조작의 배경
fn held_after(els: &[Element]) -> Option<Held> {
    els.iter().flat_map(|e| &e.ops).filter_map(|op| if let Op::W(bg, inv) = *op { Some(Held::Bg(bg, inv)) } else { None }).last()
}

/// E 한 바퀴(버스 스트레스, 읽기·쓰기 전환을 짧게 자주): 조각을 앞·뒤 절반으로 나누고,
/// 한쪽 절반의 다음 64KiB 를 먼저 읽어 들어 있어야 할 값(held — 직전 단계가 남긴 값)과 대조한 뒤 무작위 값으로 쓰기 →
/// 다른 절반에서 바로 전에 쓴 64KiB 를 읽어 대조 → 역할을 바꿔 반복. 그래서 쓰인 묶음은 쓰인 직후와 다시 덮이기 직전에 두 번 대조된다.
/// 묶음 k 의 값은 묶음 번호(첫 묶음 번호 + k)의 씨앗으로 다시 만든다(저장하지 않음). burst = 이 일꾼의 묶음 번호(이어 셈).
/// after_write(시작, 끝) 은 묶음을 쓴 직후 불린다. 반환: 대조한 칸 수
fn e_sweep<C: Cells + ?Sized>(c: &mut C, base: usize, burst: &mut u64, held: Held, mut after_write: impl FnMut(usize, usize)) -> Result<u64, Miss> {
    let n = c.len();
    let half = n / 2 / LINE_WORDS * LINE_WORDS;
    let blocks = half.div_ceil(E_BLOCK);
    // 묶음 k: 짝수는 앞 절반, 홀수는 뒤 절반의 k / 2 번째 64KiB
    let block = |k: usize| {
        let lo = (k % 2) * half + (k / 2) * E_BLOCK;
        (lo, (lo + E_BLOCK).min((k % 2) * half + half))
    };
    let first = *burst;
    let bg = |k: usize| e_bg(first + k as u64);
    let mut verified = 0u64;
    // 묶음 [lo, hi) 를 배경 b(inv 면 뒤집은 값)와 대조
    let mut check = |c: &mut C, lo: usize, hi: usize, b: Bg, inv: bool| {
        for i in lo..hi {
            let want = value(b, base + i) ^ mask(inv);
            let got = c.read(i);
            if got != want {
                return Err(Miss { i, done: verified as usize, want, got, bg: b });
            }
            verified += 1;
        }
        Ok(())
    };
    for k in 0..=2 * blocks {
        if k < 2 * blocks {
            let (lo, hi) = block(k);
            // 덮어쓰기 전에: 이 묶음에 남아 있어야 할 값과 대조 (쉬는 동안 바뀐 칸을 잡는다)
            match held {
                Held::Bg(b, inv) => check(c, lo, hi, b, inv)?,
                Held::E(f) => check(c, lo, hi, e_bg(f + k as u64), false)?,
                Held::Unknown => {}
            }
            let b = bg(k);
            for i in (lo..hi).step_by(LINE_WORDS) {
                let m = LINE_WORDS.min(hi - i);
                let mut v = [0u64; LINE_WORDS];
                for (j, x) in v[..m].iter_mut().enumerate() {
                    *x = value(b, base + i + j);
                }
                c.write_line(i, &v[..m]);
            }
            after_write(lo, hi);
            *burst += 1;
        }
        if k > 0 {
            let (lo, hi) = block(k - 1);
            check(c, lo, hi, bg(k - 1), false)?;
        }
    }
    Ok(verified)
}

/// 시험 전용: 이 크기(MB)로 돌 때 일꾼 1 이 B 의 원소 2 에서 패닉한다 (0 = 꺼짐)
#[cfg(test)]
static PANIC_WHEN_MB: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct WorkerOut {
    pinned: bool,
    passes: u64,
    verified: u64,
    /// 이 일꾼이 잡은 오류(단계마다 많아야 하나)와 패닉
    errors: Vec<MemError>,
    /// 기본 세트를 끝낸 시각(ms)
    base_ms: Option<u64>,
    /// 끝낸 기본 세트 원소들의 칸당 조작 수 합 (BASE_OPS 면 다 끝냄)
    base_ops: u64,
    /// 기본 세트 첫 원소를 끝낸 시각(ms)과 그 조작 수 — 새 버퍼의 첫 쓰기라 느리다
    base_first: Option<(u64, u64)>,
    /// 기본 세트 원소를 마지막으로 끝낸 시각(ms)
    base_last_ms: u64,
    /// 지금 도는 단계 이름·원소 번호·단계 시작 시각 (패닉 보고용)
    at: (&'static str, usize, u64),
    rounds_d: u64,
    bursts_e: u64,
}

/// 버퍼(검사 영역) 시작 포인터 — 일꾼들이 나눠 쓴다. 같은 때 같은 조각을 두 일꾼이 만지지 않는다(조각 맡기는 단계 사이 대기에서만 바뀐다)
struct Region(*mut u64);
unsafe impl Send for Region {}
unsafe impl Sync for Region {}

/// 일꾼들이 함께 쓰는 것
struct Shared<'a> {
    cfg: &'a MemConfig,
    start: Instant,
    stop: AtomicBool,
    /// 일꾼 모두가 잡은 오류 수 — max_errors 에 닿으면 stop 을 세운다
    errors: AtomicU64,
    /// 조각마다: 마지막으로 그 조각을 돈 단계가 오류로 덜 끝났는지 (칸 값을 모름). 그 조각을 돈 일꾼이 단계 안에서 세우고(오류)·지우고(끝냄),
    /// E 는 제 조각의 깃발을 보고 지운다. 한 단계 안에서 한 조각은 한 일꾼만 돌고, 세우기·지우기와 다음 단계의 읽기 사이에는
    /// 단계 시작 대기(장벽)가 있어 Relaxed 로도 순서가 맞다
    unfinished: Vec<AtomicBool>,
    /// 다음 단계가 E 인지: 기본 세트 뒤 단계 시작마다 일꾼 0 이 대기 전에 정하고, 모두 두 대기 사이에서 읽는다
    next_e: AtomicBool,
    barrier: Barrier,
    region: Region,
    words: usize,
    starts: Vec<usize>,
}

impl Shared<'_> {
    /// 조각 c 의 (시작 칸, 칸 수)
    fn span(&self, c: usize) -> (usize, usize) {
        let end = self.starts.get(c + 1).copied().unwrap_or(self.words);
        (self.starts[c], end - self.starts[c])
    }
}

pub fn run(cfg: &MemConfig) -> MemOutcome {
    let start = Instant::now();
    let words = cfg.mb * 1024 * 1024 / 8;
    let threads = cfg.threads.clamp(1, words.max(1));
    // 검사 영역은 캐시 줄(64바이트) 경계에서 시작해야 "8칸 = 한 줄"이 맞는다 — 할당은 8바이트 정렬만 보장하므로 여유를 두고 앞을 건너뛴다
    let mut buf = vec![0u64; words + LINE_WORDS - 1];
    let skip = line_skip(buf.as_ptr() as usize);
    let sh = Shared {
        cfg,
        start,
        stop: AtomicBool::new(false),
        errors: AtomicU64::new(0),
        unfinished: (0..threads).map(|_| AtomicBool::new(false)).collect(),
        next_e: AtomicBool::new(false),
        barrier: Barrier::new(threads),
        region: Region(buf[skip..].as_mut_ptr()),
        words,
        starts: chunk_starts(words, threads),
    };
    let outs: Vec<WorkerOut> = std::thread::scope(|s| {
        let sh = &sh;
        let handles: Vec<_> = (0..threads).map(|t| s.spawn(move || worker(sh, t))).collect();
        handles.into_iter().map(|h| h.join().expect("메모리 일꾼이 죽었다")).collect()
    });
    drop(buf);
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let bytes_verified = outs.iter().map(|o| o.verified).sum();
    let base_complete = outs.iter().all(|o| o.base_ms.is_some());
    let pinned = outs.iter().all(|o| o.pinned);
    let passes = outs.iter().map(|o| o.passes).sum();
    let min_thread_passes = outs.iter().map(|o| o.passes).min().unwrap_or(0);
    let rounds_d = outs.iter().map(|o| o.rounds_d).min().unwrap_or(0);
    let bursts_e = outs.iter().map(|o| o.bursts_e).sum();
    let base_ms = if base_complete {
        outs.iter().filter_map(|o| o.base_ms).max().map(|ms| ms as f64)
    } else {
        // 못 끝냈으면 일꾼마다 늘려 잡은 예상 중 가장 긴 것 — 이미 걸린 시간보다 짧을 수는 없다
        outs.iter().filter_map(|o| o.base_first.and_then(|(fm, fo)| estimate_base_ms(fm, fo, o.base_last_ms, o.base_ops))).reduce(f64::max).map(|ms| ms.max(elapsed_ms as f64))
    };
    let mut errors: Vec<MemError> = outs.into_iter().flat_map(|o| o.errors).collect();
    errors.sort_by_key(|e| e.at_ms);
    let errors_total = errors.len() as u64;
    // 여러 일꾼이 동시에 틀리면 가장 먼저 잡은 것
    let error = errors.first().cloned();
    if cfg.max_errors <= 1 {
        errors.clear();
    } else {
        errors.truncate(32);
    }
    MemOutcome {
        bytes: words * 8,
        threads,
        pinned,
        passes,
        min_thread_passes,
        bytes_verified,
        verified_bytes_per_sec: crate::cpu::per_sec(bytes_verified, elapsed_ms),
        elapsed_ms,
        base_complete,
        base_seconds_estimate: base_ms.map(|ms| (ms / 100.0).round() / 10.0),
        rounds_d,
        bursts_e,
        error,
        errors_total,
        errors,
    }
}

/// 기본 세트를 못 끝냈을 때의 예상 ms: 첫 원소(새 버퍼의 첫 쓰기 — 윈도우에서는 1GB 에 2초 넘게 걸린다)는 걸린 그대로 두고,
/// 나머지 조작은 첫 원소 뒤에 잰 속도로 늘려 잡는다. first = (첫 원소를 끝낸 시각, 그 조작 수), last_ms·ops = 마지막으로 끝낸 시각·조작 수 합.
/// 첫 원소 뒤로 끝낸 것이 없으면 잴 수 없다
fn estimate_base_ms(first_ms: u64, first_ops: u64, last_ms: u64, ops: u64) -> Option<f64> {
    (ops > first_ops).then(|| first_ms as f64 + (BASE_OPS - first_ops) as f64 * last_ms.saturating_sub(first_ms) as f64 / (ops - first_ops) as f64)
}

/// 기본 세트 뒤 다음 단계가 E 인지: 남은 시간을 D:E = 6:4 로 나눈다 — 지금까지 E 에 쓴 시간이 D 의 4/6 보다 적으면 E (처음은 D)
fn e_turn(time_d: Duration, time_e: Duration) -> bool {
    time_e.as_secs_f64() * 6.0 < time_d.as_secs_f64() * 4.0
}

/// 단계 하나가 끝난 모양
enum StageEnd {
    Done,
    Halted,
    /// 이 단계에서 오류를 잡아 기록함. halted = 건너뛰며 기다리던 중 멈춤이 옴 — 더 기다리지 말고 끝낼 것
    Errored { halted: bool },
    Failed(MemError),
}

/// 패닉 내용을 글로
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "?".into())
}

/// 일꾼 t: 기본 세트를 한 번, 그 뒤 마감까지 D/E 를 돈다.
/// 멈춤(오류·마감·패닉)은 원소·단계 시작마다 모든 일꾼이 같은 자리에서 함께 판단한다 — 대기 두 번 사이에서 깃발을 읽으므로
/// 누구는 멈추고 누구는 다음 대기에서 영영 기다리는 일이 없다. 오류를 낸 일꾼은 그 단계의 남은 대기를 함께 거친 뒤(stage 참고) 다음 단계로 가거나
/// 멈춤을 보면 끝내고, 패닉한 일꾼은 다음 시작 대기에 한 번 더 들어간 뒤 끝낸다
fn worker(sh: &Shared, t: usize) -> WorkerOut {
    let cfg = sh.cfg;
    let ms = || sh.start.elapsed().as_millis() as u64;
    let mut out = WorkerOut {
        pinned: crate::affinity::pin_current_thread(t),
        passes: 0,
        verified: 0,
        errors: Vec::new(),
        base_ms: None,
        base_ops: 0,
        base_first: None,
        base_last_ms: 0,
        at: ("", 0, 0),
        rounds_d: 0,
        bursts_e: 0,
    };
    // 모두 함께: 마감이면 멈춤 깃발을 세우고, 대기 → 깃발·다음 단계 읽기 → 대기. 두 대기 사이에는 아무도 둘을 바꾸지 않는다
    let sync = || {
        if sh.start.elapsed() >= cfg.duration {
            sh.stop.store(true, Ordering::Relaxed);
        }
        sh.barrier.wait();
        let r = (sh.stop.load(Ordering::Relaxed), sh.next_e.load(Ordering::Relaxed));
        sh.barrier.wait();
        r
    };
    let base = base_set();
    let (mut time_d, mut time_e) = (Duration::ZERO, Duration::ZERO);
    let mut burst = 0u64;
    // 시작한 D 회차 수 — 끝냈든 틀렸든 모든 일꾼이 같은 단계마다 함께 올리므로 일꾼끼리 늘 같다 (회차 번호·조각 맡기·배경).
    // 보고하는 rounds_d 는 끝낸 회차만 센다
    let mut d_next = 0u64;
    // 조각에 지금 들어 있어야 할 값 (E 가 덮어쓰기 전에 대조하는 데 쓴다). 단계를 끝까지 돌았다고 보고 정하고,
    // 덜 끝난 조각은 Shared.unfinished 깃발로 E 가 따로 안다
    let mut held = Held::Bg(Bg::Solid, false);
    loop {
        let pass = out.passes;
        let post = pass >= STAGES;
        if post && t == 0 {
            // 남은 시간은 D:E = 6:4 — 일꾼 0 이 잰 시간으로 정한다(처음은 D, 그다음 E)
            sh.next_e.store(e_turn(time_d, time_e), Ordering::Relaxed);
        }
        let (halt, e_next) = sync();
        if halt {
            break;
        }
        let began = Instant::now();
        let e_first = burst;
        // 단계 전체(조각 고르기·주입·원소)를 패닉 울타리로 감싼다 — 어디서 패닉해도 오류로 바뀌어 아래의 대기 한 번으로 맞춰진다
        let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !post {
                let (name, els) = &base[pass as usize];
                stage(sh, t, &mut out, pass, name, els, None, &sync)
            } else if !e_next {
                stage(sh, t, &mut out, pass, "D", &d_round(d_next), Some(d_next), &sync)
            } else {
                stage_e(sh, t, &mut out, pass, &mut burst, held)
            }
        }))
        .unwrap_or_else(|p| StageEnd::Failed(panic_error(t, pass, out.at, panic_text(&*p), ms())));
        let finished = match end {
            StageEnd::Done => true,
            StageEnd::Halted => break,
            // 틀린 단계도 끝낸 단계로 센다. halted 면 다른 일꾼은 그 대기에서 멈춤을 보고 이미 빠져나갔다 — 더 기다리면 교착이라 바로 끝낸다
            StageEnd::Errored { halted: true } => {
                out.passes += 1;
                break;
            }
            StageEnd::Errored { halted: false } => false,
            StageEnd::Failed(e) => {
                // 일꾼 패닉: 계속하지 않는다
                sh.stop.store(true, Ordering::Relaxed);
                out.errors.push(e);
                out.passes += 1;
                // 다른 일꾼이 다음 시작에서 멈추도록 같은 자리의 대기에 한 번 더 들어간다
                sync();
                return out;
            }
        };
        out.passes += 1;
        held = if !post {
            held_after(&base[pass as usize].1).unwrap_or(held)
        } else if e_next {
            Held::E(e_first)
        } else {
            held_after(&d_round(d_next)).unwrap_or(held)
        };
        // 틀린 단계도 걸린 시간은 D:E 나누기에 넣는다 — 안 넣으면 늘 틀리는 칸이 있을 때 E 차례가 영영 오지 않는다
        if !post {
            if out.passes == STAGES {
                out.base_ms = Some(ms());
            }
        } else if e_next {
            time_e += began.elapsed();
        } else {
            if finished {
                out.rounds_d += 1;
            }
            d_next += 1;
            time_d += began.elapsed();
        }
    }
    out
}

/// 일꾼 t 가 이 단계에서 맡은 조각 c 의 창구와 (시작 칸, 칸 수, 버퍼 전체 칸 번호 → 조각 안 번호)
fn chunk_cells(sh: &Shared, c: usize) -> (Buf, usize) {
    let (base, n) = sh.span(c);
    let ptr = unsafe { sh.region.0.add(base) };
    debug_assert_eq!(ptr as usize % 64, 0, "조각이 캐시 줄 경계에서 시작하지 않는다");
    let local = |w: usize| (w >= base && w < base + n).then(|| w - base);
    // 일꾼은 모두 동시에 돈다
    let active = sh.starts.len();
    let couple = match sh.cfg.fault {
        Some(MemFault::CouplingUp { word, distance, bit }) => local(word).map(|at| (at, distance, 1u64 << (bit % 64))),
        _ => None,
    };
    let busy = match sh.cfg.fault {
        Some(MemFault::BusyOnly { word, bit, min_active }) if active >= min_active => local(word).map(|at| (at, 1u64 << (bit % 64))),
        _ => None,
    };
    (Buf { ptr, len: n, couple, busy }, base)
}

/// 주입: 이 단계(pass)·이 자리(late, None 이면 상관없음)에 해당하고 칸이 조각 안 범위 [range) 에 있으면 그 비트를 뒤집는다.
/// ptr = 조각 시작(버퍼 전체 칸 번호 base)
fn flip_if(sh: &Shared, ptr: *mut u64, base: usize, pass: u64, late: Option<bool>, range: (usize, usize)) {
    if let Some(j) = sh.cfg.inject.filter(|j| j.pass == pass && late.is_none_or(|l| l == j.late)) {
        if j.word >= base + range.0 && j.word < base + range.1 {
            unsafe {
                let q = ptr.add(j.word - base);
                q.write_volatile(q.read_volatile() ^ (1u64 << (j.bit % 64)));
            }
        }
    }
}

/// 틀린 비트들(want ^ got)의 줄 안 위치 q = 64·(칸 % 8) + 비트. word = 버퍼 전체 칸 번호
fn line_bits(word: usize, want: u64, got: u64) -> Vec<u32> {
    let diff = want ^ got;
    (0..64).filter(|j| diff >> j & 1 == 1).map(|j| 64 * (word % LINE_WORDS) as u32 + j).collect()
}

/// at = (단계 이름, 원소 번호, 단계 시작 시각)
fn miss_error(t: usize, pass: u64, at: (&'static str, usize, u64), m: Miss, cells: &Buf, base: usize, at_ms: u64) -> MemError {
    // 다시 읽기는 캐시가 아니라 메모리에서: 방금 읽은 줄은 캐시에 있어, 읽는 길에서 틀린 값이 그대로 다시 나올 수 있다
    let p = unsafe { cells.ptr.add(m.i) };
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::x86_64::_mm_clflush(p as *const u8);
        std::arch::x86_64::_mm_mfence();
    }
    let reread = unsafe { p.read_volatile() };
    MemError {
        thread: t,
        pass,
        stage: at.0,
        element: at.1,
        pattern: bg_name(m.bg),
        offset_bytes: (base + m.i) * 8,
        expected: format!("{:#018x}", m.want),
        actual: format!("{:#018x}", m.got),
        reread: format!("{reread:#018x}"),
        kind: if reread == m.want { "read" } else { "stored" },
        line_bits: line_bits(base + m.i, m.want, m.got),
        pass_start_ms: at.2,
        at_ms,
    }
}

fn panic_error(t: usize, pass: u64, at: (&'static str, usize, u64), text: String, at_ms: u64) -> MemError {
    MemError {
        thread: t,
        pass,
        stage: at.0,
        element: at.1,
        pattern: format!("panic: {text}"),
        offset_bytes: 0,
        expected: String::new(),
        actual: String::new(),
        reread: String::new(),
        kind: "panic",
        line_bits: vec![],
        pass_start_ms: at.2,
        at_ms,
    }
}

/// 원소 목록 단계(기본 세트 한 단계 또는 D 한 회차). 첫 원소의 시작 대기는 부른 쪽에서 이미 했다
#[allow(clippy::too_many_arguments)]
fn stage(sh: &Shared, t: usize, out: &mut WorkerOut, pass: u64, name: &'static str, els: &[Element], d: Option<u64>, sync: &dyn Fn() -> (bool, bool)) -> StageEnd {
    let ms = || sh.start.elapsed().as_millis() as u64;
    out.at = (name, 0, ms());
    let c = chunk_of(t, sh.starts.len(), d);
    let (mut cells, base) = chunk_cells(sh, c);
    let n = cells.len;
    for (e, el) in els.iter().enumerate() {
        if e > 0 && sync().0 {
            return StageEnd::Halted;
        }
        out.at.1 = e;
        #[cfg(test)]
        if PANIC_WHEN_MB.load(Ordering::Relaxed) == sh.cfg.mb && t == 1 && pass == 1 && e == 2 {
            panic!("시험용 일꾼 패닉");
        }
        if e + 1 == els.len() {
            flip_if(sh, cells.ptr, base, pass, Some(true), (0, n));
        }
        let r = if cells.couple.is_none() && cells.busy.is_none() {
            run_element(&mut Plain { ptr: cells.ptr, len: n }, base, el)
        } else {
            run_element(&mut cells, base, el)
        };
        cells.fence();
        if let Err(m) = r {
            out.verified += m.done as u64 * el.reads() * 8;
            out.errors.push(miss_error(t, pass, out.at, m, &cells, base, ms()));
            sh.unfinished[c].store(true, Ordering::Relaxed);
            if sh.errors.fetch_add(1, Ordering::Relaxed) + 1 >= sh.cfg.max_errors {
                sh.stop.store(true, Ordering::Relaxed);
            }
            // 이 단계의 남은 원소는 건너뛰되 다른 일꾼과 같은 자리에서 기다린다
            for _ in e + 1..els.len() {
                if sync().0 {
                    return StageEnd::Errored { halted: true };
                }
            }
            return StageEnd::Errored { halted: false };
        }
        out.verified += n as u64 * el.reads() * 8;
        if pass < STAGES {
            let now = ms();
            out.base_first.get_or_insert((now, el.ops.len() as u64));
            out.base_ops += el.ops.len() as u64;
            out.base_last_ms = now;
        }
        if e == 0 {
            flip_if(sh, cells.ptr, base, pass, Some(false), (0, n));
        }
    }
    sh.unfinished[c].store(false, Ordering::Relaxed);
    StageEnd::Done
}

/// E 한 바퀴 단계 (제 조각)
fn stage_e(sh: &Shared, t: usize, out: &mut WorkerOut, pass: u64, burst: &mut u64, held: Held) -> StageEnd {
    let ms = || sh.start.elapsed().as_millis() as u64;
    out.at = ("E", 0, ms());
    let (mut cells, base) = chunk_cells(sh, t);
    let first = *burst;
    // 앞 단계가 이 조각을 덜 끝냈으면 칸 값을 모른다 — 이번 바퀴만 덮기 전 대조를 건너뛴다. E 는 다 쓰고 나면 다시 알므로 깃발을 지운다
    let held = if sh.unfinished[t].swap(false, Ordering::Relaxed) { Held::Unknown } else { held };
    // 주입: 그 칸이 든 묶음을 쓴 직후
    let ptr = cells.ptr;
    let flip = |lo, hi| flip_if(sh, ptr, base, pass, None, (lo, hi));
    let r = if cells.couple.is_none() && cells.busy.is_none() {
        e_sweep(&mut Plain { ptr, len: cells.len }, base, burst, held, flip)
    } else {
        e_sweep(&mut cells, base, burst, held, flip)
    };
    cells.fence();
    out.bursts_e += *burst - first;
    match r {
        Err(m) => {
            out.verified += m.done as u64 * 8;
            out.errors.push(miss_error(t, pass, out.at, m, &cells, base, ms()));
            sh.unfinished[t].store(true, Ordering::Relaxed);
            if sh.errors.fetch_add(1, Ordering::Relaxed) + 1 >= sh.cfg.max_errors {
                sh.stop.store(true, Ordering::Relaxed);
            }
            StageEnd::Errored { halted: false }
        }
        Ok(v) => {
            out.verified += v * 8;
            StageEnd::Done
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const MB8: usize = 8 * 1024 * 1024;
    const N8: u64 = (MB8 / 8) as u64;

    fn run1(mb: usize, secs: u64, threads: usize, inject: Option<MemInject>) -> MemOutcome {
        run(&MemConfig { mb, duration: Duration::from_secs(secs), threads, inject, fault: None, max_errors: 1 })
    }

    #[test]
    fn clean_run_has_no_error() {
        let out = run(&MemConfig { mb: 1, duration: Duration::from_millis(500), threads: 1, inject: None, fault: None, max_errors: 1 });
        assert!(out.error.is_none(), "{:?}", out.error);
        assert!(out.passes >= STAGES, "기본 세트는 끝내야 한다: {}", out.passes);
        assert!(out.base_complete);
        // 한 회차 = 읽기 33n
        assert!(out.bytes_verified >= 33 * (1 << 20));
        // 일꾼 하나여도 기본 세트 뒤 D 와 E 를 번갈아 돈다. 1MB = 칸 n = 131,072, E 한 바퀴 = 64KiB 묶음 16개(묶음마다 덮기 전·쓴 뒤 대조)
        let n = (1u64 << 20) / 8;
        assert!(out.rounds_d >= 2 && out.bursts_e >= 2 * 16, "{out:?}");
        assert_eq!(out.bursts_e % 16, 0, "E 는 바퀴 중간에 멈추지 않는다");
        assert_eq!(out.passes, STAGES + out.rounds_d + out.bursts_e / 16, "끝낸 단계 = 기본 세트 + D 회차 + E 바퀴");
        // 대조한 칸 = 기본 세트 33n + D 회차마다 5n + E 묶음마다 2 × 8,192칸 + 마감에 걸린 D 회차의 끝낸 원소(읽기 0~4n)
        let rest = out.bytes_verified / 8 - (33 * n + 5 * n * out.rounds_d + 2 * E_BLOCK as u64 * out.bursts_e);
        assert!(rest.is_multiple_of(n) && rest / n <= 4, "남는 칸 {rest}");
    }

    #[test]
    fn base_set_shape() {
        let set = base_set();
        let names: Vec<_> = set.iter().map(|s| s.0).collect();
        assert_eq!(names, ["A", "B", "C0", "C1", "C2", "C3", "C4", "C5", "C6", "C7", "C8"]);
        let ops: usize = set.iter().flat_map(|s| &s.1).map(|e| e.ops.len()).sum();
        let reads: u64 = set.iter().flat_map(|s| &s.1).map(|e| e.reads()).sum();
        assert_eq!((ops as u64, reads), (BASE_OPS, 33));
        assert!(set.iter().flat_map(|s| &s.1).all(|e| e.walk == Walk::Linear), "기본 세트는 선형");
        // 모든 단계는 쓰기만 하는 원소로 시작해 읽기만 하는 원소로 끝난다 (주입 자리)
        for (name, els) in &set {
            assert!(els[0].ops.iter().all(|o| matches!(o, Op::W(..))), "{name}");
            assert!(els.last().unwrap().ops.iter().all(|o| matches!(o, Op::R(..))), "{name}");
        }
        // March C- 는 오름 2·내림 2
        let b = &set[1].1;
        assert_eq!(b.iter().map(|e| e.order).collect::<Vec<_>>(), [Order::Any, Order::Up, Order::Up, Order::Down, Order::Down, Order::Any]);
        assert_eq!(set[5].1[2], Element { order: Order::Down, walk: Walk::Linear, ops: vec![Op::R(Bg::Stripe(3), true), Op::W(Bg::Stripe(3), false)] });
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
    fn line_orders_are_permutations_and_down_is_exact_reverse() {
        for walk in [Walk::Linear, Walk::Stride] {
            for nl in [0, 1, 5, 64, 65, 200, 333] {
                let up: Vec<usize> = lines(Order::Up, walk, nl).collect();
                let mut down: Vec<usize> = lines(Order::Down, walk, nl).collect();
                assert_eq!(lines(Order::Any, walk, nl).collect::<Vec<_>>(), up);
                down.reverse();
                assert_eq!(up, down, "{walk:?} {nl}");
                let mut sorted = up.clone();
                sorted.sort();
                assert_eq!(sorted, (0..nl).collect::<Vec<_>>(), "{walk:?} {nl}: 모든 줄을 한 번씩");
            }
        }
        assert_eq!(lines(Order::Down, Walk::Linear, 4).collect::<Vec<_>>(), [3, 2, 1, 0]);
        // 쪽 = 1줄이면 줄 보폭 순서 그대로: 블록마다 0번째 줄, 그다음 1번째 줄…
        assert_eq!(lines_by(Order::Up, 1, 64, 130).take(5).collect::<Vec<_>>(), [0, 64, 128, 1, 65]);
        // 쪽 2줄 · 보폭 3쪽, 줄 11개 = 쪽 6개(마지막 쪽은 1줄), 블록 2개: 쪽 순서 0,3,1,4,2,5
        assert_eq!(lines_by(Order::Up, 2, 3, 11).collect::<Vec<_>>(), [0, 1, 6, 7, 2, 3, 8, 9, 4, 5, 10]);
        // 쪽 수가 보폭의 배수가 아님(줄 9개 = 쪽 5개, 블록 2개): 없는 쪽 6 은 건너뛴다. ⇓ 는 정확한 역순
        assert_eq!(lines_by(Order::Up, 2, 3, 9).collect::<Vec<_>>(), [0, 1, 6, 7, 2, 3, 8, 4, 5]);
        assert_eq!(lines_by(Order::Down, 2, 3, 9).collect::<Vec<_>>(), [5, 4, 8, 3, 2, 7, 6, 1, 0]);
        for (page, step) in [(1, 3), (2, 3), (3, 2), (64, 64), (5, 1)] {
            for nl in [0, 1, 7, 9, 64, 65, 200, 4097, 4161] {
                let up: Vec<usize> = lines_by(Order::Up, page, step, nl).collect();
                let mut down: Vec<usize> = lines_by(Order::Down, page, step, nl).collect();
                down.reverse();
                assert_eq!(up, down, "{page} {step} {nl}");
                let mut sorted = up.clone();
                sorted.sort();
                assert_eq!(sorted, (0..nl).collect::<Vec<_>>(), "{page} {step} {nl}: 모든 줄을 한 번씩");
            }
        }
        // 실제 보폭 회차: 4KiB 쪽(64줄) 안은 차례대로, 다음 쪽은 256KiB(64쪽) 뒤 — 쪽 130개 = 블록 3개
        let s: Vec<usize> = lines(Order::Up, Walk::Stride, 130 * 64).collect();
        assert_eq!(s[..64], (0..64).collect::<Vec<_>>()[..]);
        assert_eq!([s[64], s[127], s[128], s[192]], [64 * 64, 64 * 64 + 63, 128 * 64, 64]);
        // 줄 수가 쪽의 배수가 아니면 마지막 쪽이 짧다(줄 130개 = 쪽 3개, 마지막 2줄)
        let s: Vec<usize> = lines(Order::Up, Walk::Stride, 130).collect();
        assert_eq!(s, (0..130).collect::<Vec<_>>(), "쪽 3개는 한 블록 안이라 차례대로");
    }

    #[test]
    fn d_rounds_alternate_order_and_owner() {
        let (e0, e1, e2) = (d_round(0), d_round(1), d_round(2));
        assert!(e0.iter().all(|e| e.walk == Walk::Linear) && e1.iter().all(|e| e.walk == Walk::Stride) && e2[0].walk == Walk::Linear);
        // March C- 모양, 회차 쌍마다 같은 배경, 다음 쌍은 새 배경
        assert_eq!(e0.iter().map(|e| e.order).collect::<Vec<_>>(), [Order::Any, Order::Up, Order::Up, Order::Down, Order::Down, Order::Any]);
        assert_eq!(e0[0].ops, [Op::W(Bg::Random(splitmix64(0)), false)]);
        assert_eq!(e0[0].ops, e1[0].ops);
        assert_ne!(e0[0].ops, e2[0].ops);
        // 조각 맡기: 기본 세트는 제 조각, D 는 쌍마다 돌리고 홀수 회차는 거꾸로
        let owners = |d| (0..4).map(|t| chunk_of(t, 4, d)).collect::<Vec<_>>();
        assert_eq!(owners(None), [0, 1, 2, 3]);
        assert_eq!((owners(Some(0)), owners(Some(1))), (vec![0, 1, 2, 3], vec![3, 2, 1, 0]));
        assert_eq!((owners(Some(2)), owners(Some(3))), (vec![1, 2, 3, 0], vec![2, 1, 0, 3]));
    }

    #[test]
    fn e_sweep_writes_both_halves_and_catches_a_stuck_bit() {
        use crate::memsim::{Fault, SimMem};
        let words = 65_536 + 8;
        let mut clean = SimMem::new(words, 0, None);
        let mut burst = 5;
        let mut writes = vec![];
        let v = e_sweep(&mut clean, 0, &mut burst, Held::Bg(Bg::Solid, false), |lo, hi| writes.push((lo, hi))).unwrap();
        // 앞 절반 32,768칸·뒤 절반 32,768칸을 64KiB(8,192칸) 씩 번갈아: 묶음 8개, 덮기 전·쓴 뒤 두 번씩 대조
        assert_eq!((v, burst), (2 * 65_536, 13));
        assert_eq!(writes[..3], [(0, 8_192), (32_768, 40_960), (8_192, 16_384)]);
        assert_eq!(clean.cells()[65_536..], [0; 8], "절반 둘 밖의 칸은 건드리지 않는다");
        // 직전 단계가 뒤집은 값을 남겼으면(Held::Bg(_, true)) 뒤집은 값과 대조한다
        let mut inv = SimMem::new(words, 0, None);
        (0..words).for_each(|i| inv.write(i, !value(Bg::Hash, i)));
        assert!(e_sweep(&mut inv, 0, &mut 0, Held::Bg(Bg::Hash, true), |_, _| {}).is_ok());
        // 다음 바퀴는 직전 바퀴가 남긴 값과 대조 — 잘못 알면 바로 걸린다
        assert!(e_sweep(&mut clean, 0, &mut burst, Held::E(5), |_, _| {}).is_ok());
        assert!(e_sweep(&mut clean, 0, &mut burst, Held::E(5), |_, _| {}).is_err(), "직전 바퀴는 13 부터였다");
        // 칸 40,000 (뒤 절반의 첫 묶음 안) 의 비트 3 이 0 에 고착: 무작위 값이 그 비트에 1 을 쓰는 바퀴에서 걸린다
        let mut stuck = SimMem::new(words, 0, Some(Fault::Saf { word: 40_000, bit: 3, val: false }));
        let (mut b, mut held) = (0, Held::Bg(Bg::Solid, false));
        let m = loop {
            let first = b;
            if let Err(m) = e_sweep(&mut stuck, 0, &mut b, held, |_, _| {}) {
                break m;
            }
            held = Held::E(first);
            assert!(b < 1000);
        };
        assert_eq!((m.i, m.got ^ m.want), (40_000, 1 << 3));
    }

    #[test]
    fn e_last_block_stops_at_its_half() {
        use crate::memsim::SimMem;
        // 절반이 64KiB 의 배수가 아니면 절반마다 마지막 묶음이 짧다 — 다른 절반으로 넘어가 쓰면 그쪽 값을 망가뜨린다
        let half = E_BLOCK + LINE_WORDS;
        let mut m = SimMem::new(2 * half, 0, None);
        let mut writes = vec![];
        e_sweep(&mut m, 0, &mut 0, Held::Bg(Bg::Solid, false), |lo, hi| writes.push((lo, hi))).unwrap();
        assert_eq!(writes, [(0, E_BLOCK), (half, half + E_BLOCK), (E_BLOCK, half), (half + E_BLOCK, 2 * half)]);
    }

    #[test]
    fn e_backgrounds_differ_per_burst_and_from_d() {
        // 묶음마다 다른 무작위 값 — 같으면 옛 묶음 값이 남아 있어도 대조를 통과한다
        let e: Vec<Bg> = (0..256).map(e_bg).collect();
        for (k, b) in e.iter().enumerate() {
            assert!(!e[k + 1..].contains(b), "묶음 {k} 의 배경이 뒤에서 또 나온다");
        }
        // D 회차 배경(씨앗 splitmix64(k))과도 다르다
        assert!((0..256).all(|k| !e.contains(&Bg::Random(splitmix64(k)))));
    }

    #[test]
    fn e_catches_a_flip_in_an_idle_block_and_follows_d() {
        use crate::memsim::SimMem;
        let words = 65_536;
        // D 회차 0(선형)·1(쪽 보폭 — 8,192줄 = 쪽 128개라 순서가 실제로 섞인다)이 남긴 값 위에서 E 를 시작해도 오탐이 없다 (D → E 넘어가기)
        let mut m = SimMem::new(words, 0, None);
        for el in &d_round(0) {
            run_element(&mut m, 0, el).unwrap();
        }
        let d = d_round(1);
        for el in &d {
            run_element(&mut m, 0, el).unwrap();
        }
        let held = held_after(&d).unwrap();
        assert_eq!(held, Held::Bg(Bg::Random(splitmix64(0)), false));
        let mut burst = 0;
        e_sweep(&mut m, 0, &mut burst, held, |_, _| {}).unwrap();
        // 바퀴 사이에 쉬는 묶음(묶음 3 = 뒤 절반의 두 번째 64KiB)의 칸 하나가 바뀌면, 다음 바퀴가 덮기 전에 잡는다
        let word = 32_768 + 8_192 + 77;
        let mut cells = m.cells().to_vec();
        cells[word] ^= 1 << 9;
        let mut m = SimMem::new(words, 0, None);
        for (i, v) in cells.into_iter().enumerate() {
            m.write(i, v);
        }
        let miss = e_sweep(&mut m, 0, &mut burst, Held::E(0), |_, _| {}).unwrap_err();
        assert_eq!((miss.i, miss.want ^ miss.got), (word, 1 << 9));
        // 덮기 전 대조라서, 그 묶음을 덮기 전까지 대조한 칸 수: 묶음 0·1·2 덮기 전 + 묶음 0·1 쓴 뒤 = 5묶음 + 77
        assert_eq!(miss.done, 5 * E_BLOCK + 77);
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
        // 주입은 칸에 남는다 → stored, 칸 12,345 = 줄 안 칸 1 → 위치 64 + 17
        assert_eq!((e.kind, e.line_bits), ("stored", vec![81]));
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
    fn injection_in_d_and_e_is_caught() {
        // 기본 세트 뒤 첫 단계는 D(순번 11), 그다음 E(12)
        let inj = MemInject { pass: STAGES, word: 7, bit: 0, late: false };
        let e = run1(1, 10, 1, Some(inj)).error.expect("D 주입을 잡아야 한다");
        assert_eq!((e.pass, e.stage, e.element, e.offset_bytes, e.pattern.as_str()), (STAGES, "D", 1, 56, "random"));
        // E: 그 칸이 든 64KiB 묶음을 쓴 직후 넣고, 다음 묶음 차례의 대조에서 잡힌다. 일꾼 2 → 칸 600,000 은 일꾼 1 조각의 앞 절반
        let inj = MemInject { pass: STAGES + 1, word: 600_000, bit: 40, late: true };
        let out = run1(8, 10, 2, Some(inj));
        let e = out.error.clone().expect("E 주입을 잡아야 한다");
        assert_eq!((e.thread, e.pass, e.stage, e.offset_bytes), (1, STAGES + 1, "E", 600_000 * 8));
        assert_eq!(u64::from_str_radix(&e.expected[2..], 16).unwrap() ^ u64::from_str_radix(&e.actual[2..], 16).unwrap(), 1 << 40);
        assert!(out.bursts_e > 0 && out.rounds_d == 1, "{out:?}");
        // 조각 n = 524,288칸, 절반 262,144칸 = 묶음 32개. 칸 600,000 = 조각 안 75,712 = 앞 절반 9번째 묶음 → 묶음 차례 k = 18.
        // 묶음 18 을 쓴 직후 뒤집혔으므로, 기대값은 덮기 전 값(D 배경)이 아니라 E 가 쓴 묶음 18 의 값이다
        assert_eq!(e.expected, format!("{:#018x}", value(e_bg(18), 600_000)));
        // 일꾼 1 은 묶음 19 를 쓴 뒤 묶음 18 을 대조하다 잡는다: 덮기 전 대조 20묶음 + 쓴 뒤 대조 18묶음 + 묶음 18 안 1,984칸.
        // 일꾼 0 은 E 한 바퀴(묶음 64개)를 끝낸 뒤 다음 단계 시작에서 멈춘다
        let n = 524_288u64;
        let e1 = 38 * E_BLOCK as u64 + 1_984;
        assert_eq!(out.bytes_verified, ((33 + 5 + 2) * n + (33 + 5) * n + e1) * 8);
        assert_eq!(out.bursts_e, 64 + 20);
    }

    #[test]
    fn base_incomplete_is_reported_with_estimate() {
        let out = run(&MemConfig { mb: 256, duration: Duration::from_millis(150), threads: 1, inject: None, fault: None, max_errors: 1 });
        assert!(out.error.is_none());
        assert!(!out.base_complete);
        // 첫 원소 뒤로 끝낸 원소가 없으면(느린 첫 쓰기) 예상은 없다 — 계산식은 base_estimate_keeps_first_element_as_is 가 고정
        if let Some(est) = out.base_seconds_estimate {
            assert!(est * 1000.0 >= out.elapsed_ms as f64 - 100.0, "예상 {est}초 < 걸린 {}ms", out.elapsed_ms);
        }
        assert_eq!((out.rounds_d, out.bursts_e), (0, 0));
    }

    #[test]
    fn error_halts_other_workers_at_next_element() {
        // 일꾼 2, 8MB: 일꾼 1 조각의 칸 100 을 B 첫 원소 직후 뒤집으면 일꾼 1 은 B 원소 1 에서 잡는다.
        // 일꾼 0 은 원소마다 모두를 기다리므로 B 원소 2 시작에서 멈춘다 — B 를 끝까지 돌지 않는다
        let n = (MB8 / 8 / 2) as u64;
        let inj = MemInject { pass: 1, word: n as usize + 100, bit: 2, late: false };
        let out = run1(8, 10, 2, Some(inj));
        let e = out.error.clone().expect("주입한 오류를 잡아야 한다");
        assert_eq!((e.thread, e.stage, e.element), (1, "B", 1));
        // 끝낸 단계: 일꾼 0 은 A, 일꾼 1 은 A + 틀린 B. 대조한 칸: 일꾼 0 은 A 읽기 n + B 원소 1 n, 일꾼 1 은 A n + B 원소 1 의 앞 100칸
        assert_eq!((out.passes, out.bytes_verified), (3, (3 * n + 100) * 8));
    }

    #[test]
    fn base_estimate_after_an_early_stop() {
        // C0 원소 1 에서 오류로 멈추면 기본 세트는 미완료 — 끝낸 13조작(A 2 + B 10 + C0 첫 원소 1)의 속도로 66조작을 늘려 잡는다.
        // 첫 원소 뒤 12조작에 걸린 시간을 65조작으로 늘리므로 걸린 시간보다 훨씬 길다(맥 256MB: 약 1.0초 대 0.2초, 첫 쓰기가 느린 윈도우도 2배 넘게).
        // 예상은 0.1초 단위로 반올림되므로 100ms 를 더 얹어 비교한다
        let out = run1(256, 30, 1, Some(MemInject { pass: 2, word: 0, bit: 0, late: false }));
        assert!(out.failed() && !out.base_complete);
        let est = out.base_seconds_estimate.expect("끝낸 원소로 예상을 내야 한다");
        // 아래 한계: 예상은 걸린 시간 밑으로 내려가지 않게 바닥을 두므로(ms.max(elapsed)), 늘려 잡기가 빠지면 예상 ≈ 걸린 시간(반올림 ±50ms)이다.
        // 늘려 잡으면 첫 원소 뒤 12조작 시간의 53/12 배만큼 더 길다(맥 256MB 에서 약 0.7초). 예전의 "걸린 시간의 1.5배"는 부하로
        // 첫 원소(새 버퍼 첫 쓰기)만 길어지면 깨질 수 있어, 반올림보다 넉넉한 200ms 를 넘는지로 본다 — 계산식 자체는 base_estimate_keeps_first_element_as_is 가 고정
        assert!(est * 1000.0 >= out.elapsed_ms as f64 + 200.0, "예상 {est}초, 걸린 {}ms", out.elapsed_ms);
        // 늘려 잡아도 65/12 배(약 5.4배)를 넘지 않는다 — 초 단위 바꾸기가 틀리면 크게 벗어난다
        assert!(est * 1000.0 <= 10.0 * out.elapsed_ms as f64 + 100.0, "예상 {est}초, 걸린 {}ms", out.elapsed_ms);
    }

    #[test]
    fn d_and_e_share_time_six_to_four() {
        let ms = Duration::from_millis;
        assert!(!e_turn(ms(0), ms(0)), "처음은 D");
        assert!(e_turn(ms(3_000), ms(1_999)));
        assert!(!e_turn(ms(3_000), ms(2_000)), "E 가 D 의 4/6 에 닿으면 D");
    }

    #[test]
    fn base_estimate_keeps_first_element_as_is() {
        // 첫 원소(조작 1)가 2.3초(새 버퍼 첫 쓰기), 그 뒤 조작 1개에 0.1초 → 2.3 + 65 × 0.1 = 8.8초 (66배로 늘리지 않는다)
        assert_eq!(estimate_base_ms(2_300, 1, 2_400, 2), Some(8_800.0));
        // 조작 34개까지 1초 더: 0.5 + 65 × 1/33
        assert_eq!(estimate_base_ms(500, 1, 1_500, 34), Some(500.0 + 65_000.0 / 33.0));
        // 첫 원소뿐이면 속도를 잴 수 없다
        assert_eq!(estimate_base_ms(2_300, 1, 2_300, 1), None);
    }

    #[test]
    fn line_bits_are_positions_in_the_cache_line() {
        // 칸 10 = 줄 안 칸 2 → 위치 128 + 비트
        assert_eq!(line_bits(10, 0, 0b101 | 1 << 63), [128, 130, 191]);
        assert_eq!(line_bits(7, u64::MAX, u64::MAX), Vec::<u32>::new());
        assert_eq!(line_bits(8, 1 << 5, 0), [5]);
    }

    #[test]
    fn base_complete_then_d_and_e_run() {
        let out = run(&MemConfig { mb: 2, duration: Duration::from_millis(800), threads: 2, inject: None, fault: None, max_errors: 1 });
        assert!(out.error.is_none(), "{:?}", out.error);
        assert!(out.base_complete);
        assert!(out.base_seconds_estimate.unwrap() <= out.elapsed_ms as f64 / 1000.0 + 0.1);
        assert!(out.rounds_d >= 1 && out.bursts_e >= 1, "{out:?}");
        assert_eq!(out.passes, 2 * out.min_thread_passes);
        assert!(out.min_thread_passes > STAGES);
    }

    #[test]
    fn panicking_worker_stops_the_run_instead_of_hanging() {
        PANIC_WHEN_MB.store(3, Ordering::Relaxed);
        let t = Instant::now();
        let out = run(&MemConfig { mb: 3, duration: Duration::from_secs(30), threads: 3, inject: None, fault: None, max_errors: 1 });
        PANIC_WHEN_MB.store(0, Ordering::Relaxed);
        assert!(t.elapsed() < Duration::from_secs(20), "패닉 뒤 다른 일꾼이 대기에서 멈췄다");
        let e = out.error.clone().expect("패닉은 오류로 남아야 한다");
        assert_eq!((e.thread, e.pass, e.stage, e.element, e.pattern.as_str(), e.kind), (1, 1, "B", 2, "panic: 시험용 일꾼 패닉", "panic"));
        assert!(out.failed());
    }

    // 버퍼 밖 위치의 주입은 무시한다 (버퍼 밖에 쓰면 안 된다)
    #[test]
    fn out_of_range_injection_is_ignored() {
        let inj = MemInject { pass: 0, word: MB8 / 8, bit: 0, late: false };
        let out = run(&MemConfig { mb: 8, duration: Duration::from_millis(300), threads: 1, inject: Some(inj), fault: None, max_errors: 1 });
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
        // 캐시 줄(8칸) 경계에 맞춘다
        assert_eq!(chunk_starts(800, 3), vec![0, 264, 528]);
        assert_eq!(chunk_starts(64, 4), vec![0, 16, 32, 48]);
        assert_eq!(chunk_starts(5, 1), vec![0]);
        assert_eq!(chunk_starts(10, 3), vec![0, 0, 0], "줄이 모자라면 앞 일꾼은 빈 조각");
    }

    #[test]
    fn many_workers_cover_their_chunks() {
        // 진행만 보는 시험이라 마감을 넉넉히: 일꾼 넷이 원소마다 서로를 기다리므로 다른 시험과 겹친 윈도우 러너(4코어)에서는
        // 한 코어의 차례가 밀려 0.5초 안에 단계 A 도 못 끝낸 적이 있다
        let out = run(&MemConfig { mb: 8, duration: Duration::from_secs(5), threads: 4, inject: None, fault: None, max_errors: 1 });
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
        // 8MB = 1,048,576 칸, 일꾼 3명 → 시작 0 / 349,520 / 699,040 (줄 경계), 마지막 칸은 일꾼 2
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
        let out = run(&MemConfig { mb: 256, duration: Duration::from_millis(200), threads: 3, inject: None, fault: None, max_errors: 1 });
        assert!(out.error.is_none());
        assert!(t.elapsed() < Duration::from_secs(20));
        assert_eq!(out.passes % 3, 0, "일꾼마다 같은 수의 단계");
    }

    #[test]
    fn tested_region_starts_on_a_cache_line() {
        for addr in (0x1000..0x1040).step_by(8) {
            let skip = line_skip(addr);
            assert!(skip < LINE_WORDS);
            assert_eq!((addr + skip * 8) % 64, 0, "{addr:#x}");
        }
        assert_eq!((line_skip(0x1000), line_skip(0x1008), line_skip(0x1038)), (0, 7, 1));
    }

    #[test]
    fn zero_workers_becomes_one() {
        let out = run(&MemConfig { mb: 1, duration: Duration::from_millis(100), threads: 0, inject: None, fault: None, max_errors: 1 });
        assert_eq!(out.threads, 1);
    }

    #[test]
    fn e_goes_through_fault_hooks() {
        // 고장 흉내는 기본 세트가 먼저 잡아 run 으로는 E 까지 못 간다 — E 단계를 바로 불러 갈고리 창구로 도는지 본다
        let cfg = MemConfig { mb: 1, duration: Duration::ZERO, threads: 1, inject: None, fault: Some(MemFault::BusyOnly { word: 100, bit: 5, min_active: 1 }), max_errors: 1 };
        let words = 1024 * 1024 / 8;
        let mut buf = vec![0u64; words + LINE_WORDS - 1];
        let skip = line_skip(buf.as_ptr() as usize);
        let sh = Shared { cfg: &cfg, start: Instant::now(), stop: AtomicBool::new(false), next_e: AtomicBool::new(false), barrier: Barrier::new(1), region: Region(buf[skip..].as_mut_ptr()), words, starts: vec![0], errors: AtomicU64::new(0), unfinished: vec![AtomicBool::new(false)] };
        let mut out = WorkerOut { pinned: false, passes: 0, verified: 0, errors: vec![], base_ms: None, base_ops: 0, base_first: None, base_last_ms: 0, at: ("", 0, 0), rounds_d: 0, bursts_e: 0 };
        // 버퍼는 0 으로 채워져 있다 = 배경 0 이 남아 있는 상태
        let StageEnd::Errored { halted: false } = stage_e(&sh, 0, &mut out, STAGES + 1, &mut 0, Held::Bg(Bg::Solid, false)) else { panic!("E 가 바쁠 때만 틀리는 칸을 못 잡음") };
        let e = &out.errors[0];
        assert_eq!((e.stage, e.offset_bytes, e.kind), ("E", 100 * 8, "read"));
    }

    fn run_keep(mb: usize, secs: u64, threads: usize, max_errors: u64, fault: Option<MemFault>) -> MemOutcome {
        run(&MemConfig { mb, duration: Duration::from_secs(secs), threads, inject: None, fault, max_errors })
    }

    // 늘 틀리는 칸: 읽을 때마다 비트 3 이 틀린다(일꾼 1개 이상이면)
    const STUCK: MemFault = MemFault::BusyOnly { word: 1000, bit: 3, min_active: 1 };

    #[test]
    fn default_stops_at_first_error() {
        let out = run_keep(1, 5, 1, 1, Some(STUCK));
        assert_eq!(out.errors_total, 1);
        assert!(out.errors.is_empty(), "기본 실행은 목록을 채우지 않는다");
        assert_eq!(out.error.as_ref().unwrap().offset_bytes, 8000);
        assert!(out.elapsed_ms < 4000, "첫 오류에서 멈춰야 한다: {}", out.elapsed_ms);
    }

    #[test]
    fn keep_going_stops_at_n() {
        // 7 = 기본 세트 11단계와 어긋나게 — 단계 가운데에서 N 에 닿는다
        let out = run_keep(1, 20, 1, 7, Some(STUCK));
        assert_eq!(out.errors_total, 7);
        assert_eq!(out.errors.len(), 7);
        assert_eq!(out.error.as_ref(), out.errors.first());
        // 단계마다 하나씩, 단계 순번이 늘어난다
        assert!(out.errors.windows(2).all(|w| w[0].pass < w[1].pass), "{:?}", out.errors.iter().map(|e| e.pass).collect::<Vec<_>>());
        assert!(out.elapsed_ms < 15_000, "N 에 닿으면 멈춰야 한다");
    }

    #[test]
    fn keep_going_reaches_d_and_e_without_false_errors() {
        // 기본 세트 11 + D·E 몇 단계: 틀린 칸 말고는 오류가 없어야 한다(덜 끝난 단계 뒤 E 의 덮기 전 대조를 건너뛰는지)
        let out = run_keep(1, 20, 1, 20, Some(STUCK));
        assert_eq!(out.errors_total, 20);
        assert!(out.errors.iter().all(|e| e.offset_bytes == 8000), "{:?}", out.errors.iter().map(|e| (e.stage, e.offset_bytes)).collect::<Vec<_>>());
        assert!(out.errors.iter().any(|e| e.stage == "D") && out.errors.iter().any(|e| e.stage == "E"));
    }

    #[test]
    fn keep_going_two_workers_no_cascade() {
        // 칸 1000 은 일꾼 0 의 조각(1MB = 131,072칸, 일꾼 2 → 조각 65,536칸). 일꾼 0 만 계속 틀리고 일꾼 1 은 정상 — 교착 없이 끝나야 한다
        let out = run_keep(1, 20, 2, 12, Some(STUCK));
        assert!(out.errors_total >= 12, "{}", out.errors_total);
        assert!(out.errors.iter().all(|e| e.offset_bytes == 8000));
        assert!(out.min_thread_passes >= 1);
    }

    #[test]
    fn keep_going_two_workers_through_rotated_d_and_e() {
        // 늘 틀리는 칸을 일꾼 1 의 조각(칸 65,536..131,072)에 둔다. D:E 차례는 일꾼 0 이 잰 시간으로 정하므로, 일꾼 0 의 단계가
        // 제 조각에서 틀리지 않아야 시간이 고르게 쌓여 D 회차가 여러 번 온다(일꾼 0 조각이면 E 가 일찍 틀려 짧아져 뒤 단계가 거의 E 로 채워진다)
        let word = 65_536 + 1_000;
        let fault = MemFault::BusyOnly { word, bit: 3, min_active: 1 };
        // 오류 30개 = 기본 세트 11 + 뒤 단계 19 (단계마다 하나: D 는 그 회차에 조각 1 을 맡은 일꾼, E 는 일꾼 1).
        // D 회차 1·2 는 일꾼 0 이 조각 1 을 맡아 덜 끝내고(쪽 보폭 순서로 쪽 0·64 를 덮은 뒤 쪽 1 에서 틀림), 그 뒤 E 에서 일꾼 1 이 그 조각을 돈다 —
        // 조각 깃발이 없으면 덮기 전 대조가 옛 값을 오류로 잡고(틀린 칸이 아닌 곳), 일꾼끼리 D 회차 번호가 어긋나면 같은 조각을 둘이 돌아 거짓 오류가 난다
        let out = run_keep(1, 20, 2, 30, Some(fault));
        assert!(out.errors_total >= 30, "{}", out.errors_total);
        assert!(out.errors.iter().all(|e| e.offset_bytes == word * 8), "{:?}", out.errors.iter().map(|e| (e.thread, e.stage, e.offset_bytes)).collect::<Vec<_>>());
        let summary = out.errors.iter().map(|e| (e.thread, e.stage, e.pass)).collect::<Vec<_>>();
        let d0 = out.errors.iter().filter(|e| e.stage == "D" && e.thread == 0).map(|e| e.pass).min();
        let d0 = d0.unwrap_or_else(|| panic!("일꾼 0 이 조각 1 을 맡은 D 회차가 있어야 한다: {summary:?}"));
        assert!(out.errors.iter().any(|e| e.stage == "E" && e.pass > d0), "그 뒤 E 가 있어야 한다: {summary:?}");
        assert!(out.elapsed_ms < 15_000, "N 에 닿으면 멈춰야 한다(교착 없음)");
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
