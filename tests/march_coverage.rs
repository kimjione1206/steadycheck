//! 고장 모형 시뮬레이터: 실제 검사 코드(기본 세트 실행기)를 고장 하나 품은 작은 가짜 메모리에 돌려 고장 종류별 검출률을 잰다.
//! "100%" 는 모두 이 파일의 고장 목록(memsim::fault_catalog·coupling_bundle)과 memsim 의 고장 정의 기준이다 —
//! 특히 데이터선 단락(LineShort)은 "읽을 때 두 자리가 합쳐짐"으로 우리가 정한 정의다.
//! 실행기는 캐시 줄(8칸) 단위로 읽고 쓰며, 줄 쓰기(write_line)는 ⇓ 원소에서도 줄 안 칸 번호 오름차순으로 한꺼번에 넘긴다.
//! SimMem 은 줄 쓰기에서 "같은 줄에 동시에 쓰인 칸끼리는 결합 효과가 그 쓰기를 이긴다"고 정의한다(칸 안 비트끼리와 같은 규칙) —
//! 칸마다 차례로 쓰는 정의였다면 기본 세트가 같은 줄 다른 칸 멱등 결합 26/16,128 건을 놓친다.
//! 일꾼 여럿이면 ⇓ 가 조각 안에서만 순서를 뒤집으므로 조각 사이 쌍은 보장 밖이다(속도 차이가 나면 늘어난다 —
//! workers_with_barrier_and_their_gap). 남은 시간의 D 회차가 조각 맡기를 돌려 이 틈을 메운다(d_rounds_close_the_cross_chunk_gap).

use std::time::Instant;
use steadycheck::mem::{base_set, chunk_of, chunk_starts, d_round, lines, run_element, step, Bg, Cells, Element, Miss, Op, Order, Walk, LINE_WORDS};
use steadycheck::memsim::{coupling_bundle, coverage, coverage_of, fault_catalog, Fault, SimMem};

/// 시뮬레이터 메모리: 64칸 = 캐시 줄 8개
const WORDS: usize = 64;

/// 기본 세트를 일꾼 하나로 돌려 어긋남이 나오면 true
fn base(c: &mut dyn Cells) -> bool {
    base_set().iter().flat_map(|s| &s.1).any(|el| run_element(c, 0, el).is_err())
}

/// 큰 메모리의 한 조각만 보이는 창구 — 실제 일꾼이 자기 조각을 보는 것과 같다
struct View<'a> {
    m: &'a mut dyn Cells,
    start: usize,
    len: usize,
}

impl Cells for View<'_> {
    fn len(&self) -> usize {
        self.len
    }
    fn read(&mut self, i: usize) -> u64 {
        self.m.read(self.start + i)
    }
    fn write(&mut self, i: usize, v: u64) {
        self.m.write(self.start + i, v)
    }
    fn write_line(&mut self, i: usize, v: &[u64]) {
        self.m.write_line(self.start + i, v)
    }
}

/// 일꾼 여럿 대기 모형: 칸을 실제 실행처럼 조각으로 나누고, 원소마다 차례(turn)를 돌며 일꾼 t 가 한 차례에 speeds[t] 줄씩 처리한다.
/// 원소 끝 = 전원 대기. 속도가 다른 일꾼(성능·효율 코어, 대역폭 몫 차이)은 흔하므로 speeds 로 흉내 낸다.
/// 기본 세트 뒤 D 를 d_rounds 회차 이어 돈다(실제와 같이 홀수 회차는 조각을 거꾸로 맡는다).
/// mirror 면 ⇓ 원소에서 일꾼 순서를 거꾸로 돈다 — 전체 방문 순서가 ⇑ 의 정확한 역순이 되는 **이상화된 일정**으로,
/// 병렬로 도는 실제 일꾼에게는 일어날 수 없다(비교용)
fn turns(speeds: &'static [usize], mirror: bool, d_rounds: u64) -> impl Fn(&mut dyn Cells) -> bool {
    move |c| {
        let (n, workers) = (c.len(), speeds.len());
        let starts = chunk_starts(n, workers);
        let lens: Vec<usize> = (0..workers).map(|t| starts.get(t + 1).copied().unwrap_or(n) - starts[t]).collect();
        let mut program: Vec<(Element, Option<u64>)> = base_set().into_iter().flat_map(|s| s.1).map(|e| (e, None)).collect();
        for r in 0..d_rounds {
            program.extend(d_round(r).into_iter().map(|e| (e, Some(r))));
        }
        program.iter().any(|(el, d)| {
            let order: Vec<usize> = if mirror && el.order == Order::Down { (0..workers).rev().collect() } else { (0..workers).collect() };
            let chunk: Vec<usize> = (0..workers).map(|t| chunk_of(t, workers, *d)).collect();
            let mut todo: Vec<Vec<usize>> = chunk.iter().map(|&k| lines(el.order, el.walk, lens[k].div_ceil(LINE_WORDS)).collect()).collect();
            todo.iter_mut().for_each(|l| l.reverse());
            while todo.iter().any(|l| !l.is_empty()) {
                for &t in &order {
                    for _ in 0..speeds[t] {
                        if let Some(line) = todo[t].pop() {
                            let k = chunk[t];
                            if step(&mut View { m: &mut *c, start: starts[k], len: lens[k] }, starts[k], el, line, 0).is_err() {
                                return true;
                            }
                        }
                    }
                }
            }
            false
        })
    }
}

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 0.4.0 의 패스(⇑(w p);⇑(r p,w p̄);⇑(r p̄), 6무늬 회전)를 그대로 옮긴 비교용 복제 — 실행 경로에서는 사라졌다
fn old_pass(c: &mut dyn Cells, pass: u64) -> bool {
    let p = |i: usize| match pass % 6 {
        0 => 0x5555_5555_5555_5555,
        1 => 0xAAAA_AAAA_AAAA_AAAA,
        2 => 0,
        3 => u64::MAX,
        4 => (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (0xD1B5_4A32_D192_ED03 ^ pass),
        _ => splitmix64(splitmix64(0x2545_F491_4F6C_DD1D ^ pass).wrapping_add(i as u64)),
    };
    let n = c.len();
    (0..n).for_each(|i| c.write(i, p(i)));
    for i in 0..n {
        if c.read(i) != p(i) {
            return true;
        }
        c.write(i, !p(i));
    }
    (0..n).any(|i| c.read(i) != !p(i))
}

fn old(rounds: u64) -> impl Fn(&mut dyn Cells) -> bool {
    move |c| (0..6 * rounds).any(|pass| old_pass(c, pass))
}

fn rate(rows: &[(&str, usize, usize)], kind: &str) -> (usize, usize) {
    let r = rows.iter().find(|r| r.0 == kind).unwrap_or_else(|| panic!("{kind} 없음"));
    (r.1, r.2)
}

fn print_table(title: &str, rows: &[(&str, usize, usize)]) {
    eprintln!("{title}");
    for (kind, caught, total) in rows {
        eprintln!("  {kind:<13} {caught:>7}/{total:<7} {:>7.2}%", 100.0 * *caught as f64 / *total as f64);
    }
}

const KINDS: [&str; 11] = ["SAF", "TF", "AF-alias", "AF-none", "CFin/inter", "CFin/intra", "CFid/inter", "CFid/intra", "CFst/inter", "CFst/intra", "LineShort"];

#[test]
fn base_set_catches_whole_catalog_as_defined() {
    let t = Instant::now();
    let rows = coverage(base, WORDS);
    print_table(&format!("기본 세트 A+B+C, 일꾼 1, 워드당 66번 접근 — 이 고장 목록·정의 기준 ({:.2}초):", t.elapsed().as_secs_f64()), &rows);
    assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), KINDS);
    for (k, c, n) in rows {
        assert_eq!(c, n, "기본 세트가 {k} 를 놓쳤다");
    }
}

#[test]
fn base_set_catches_coupling_bundle_on_every_bit_pair() {
    // 워드 쌍 4개(같은 줄 / 다른 줄 × 가해 칸이 앞 / 뒤) × 비트 쌍 64×64 전부 × 결합 종류·방향 전부
    let pairs = [(1, 6), (6, 1), (9, 50), (50, 9)];
    let faults = coupling_bundle(&pairs);
    assert_eq!(faults.len(), 4 * 4096 * 10);
    let t = Instant::now();
    let rows = coverage_of(base, WORDS, faults);
    print_table(&format!("기본 세트, 비트 축 전수 묶음 {}건 — 이 목록·정의 기준 ({:.2}초):", 4 * 4096 * 10, t.elapsed().as_secs_f64()), &rows);
    assert_eq!(rows, [("CFin/all-bits", 32_768, 32_768), ("CFid/all-bits", 65_536, 65_536), ("CFst/all-bits", 65_536, 65_536)]);
}

/// 워드 간 결합만 (일꾼 사이 순서에 달린 고장)
fn inter_only(words: usize) -> Vec<(&'static str, Fault)> {
    fault_catalog(words).into_iter().filter(|f| f.0.ends_with("/inter")).collect()
}

/// 같은 비트끼리 워드 간 멱등 결합: 모든 워드 순서쌍 × 비트 9 × 방향 2 × 강제값 2 — 줄 안 위치만 다르고 줄무늬 비트는 같은 쌍이 많다
fn same_bit_cfid(words: usize) -> Vec<(&'static str, Fault)> {
    let mut out = Vec::new();
    for a in 0..words {
        for v in (0..words).filter(|&v| v != a) {
            for rising in [false, true] {
                for force in [false, true] {
                    out.push(("CFid/same-bit", Fault::CfId { agg: (a, 9), vic: (v, 9), rising, force }));
                }
            }
        }
    }
    out
}

fn missed(rows: &[(&str, usize, usize)]) -> Vec<(String, usize)> {
    rows.iter().map(|(k, c, n)| (k.to_string(), n - c)).collect()
}

#[test]
fn workers_with_barrier_and_their_gap() {
    // 같은 속도 일꾼 4: 이 목록 전 종류 100%
    let equal = coverage(turns(&[1, 1, 1, 1], false, 0), WORDS);
    print_table("기본 세트, 일꾼 4 대기 모형(같은 속도) — 이 고장 목록·정의 기준:", &equal);
    for (k, c, n) in &equal {
        assert_eq!(c, n, "같은 속도 일꾼 4 모형이 {k} 를 놓쳤다");
    }
    // 이상화(실제로는 일어날 수 없는) 정확한 역순 일정: 워드 간 결합 100%
    let mirror = coverage_of(turns(&[1, 1, 1, 1], true, 0), WORDS, inter_only(WORDS));
    print_table("비교: 이상화 일정(⇓ 에서 일꾼 순서까지 거꾸로, 실현 불가) — 워드 간 결합:", &mirror);
    assert!(mirror.iter().all(|(_, c, n)| c == n));
    // 보장 밖(정확히): ⇓ 는 조각 안에서만 순서를 뒤집으므로, 서로 다른 조각의 쌍은 속도와 상관없이 ⇑ 와 ⇓ 에서 방문 시간 순서가 같아질 수 있다.
    // 줄 안 위치·비트가 같아 줄무늬로도 못 가르는 쌍은 그때 멱등 결합을 놓친다. 속도 차이가 나면 그런 쌍이 늘어난다 — D 가 메울 대상
    let mut seen = Vec::new();
    for speeds in [&[1, 2][..], &[1, 1, 1, 2][..], &[1, 3][..], &[1, 100][..]] {
        let rows = coverage_of(turns(speeds, false, 0), WORDS, inter_only(WORDS));
        print_table(&format!("기본 세트, 일꾼 속도 {speeds:?} — 워드 간 결합(64칸 목록):"), &rows);
        seen.push(missed(&rows));
    }
    for (words, speeds) in [(128, &[1, 1][..]), (128, &[1, 2][..]), (128, &[1, 1, 1, 2][..])] {
        let rows = coverage_of(turns(speeds, false, 0), words, same_bit_cfid(words));
        print_table(&format!("기본 세트, 일꾼 속도 {speeds:?} — 같은 비트 워드 간 멱등 결합({words}칸, {}건):", rows[0].2), &rows);
        seen.push(missed(&rows));
    }
    // 실측 고정(놓친 수): 64칸 목록은 속도 [1,2]·[1,1,1,2] 에서 0, [1,3]·[1,100] 에서 멱등 결합 2건.
    // 같은 비트 쌍(128칸 65,024건)은 같은 속도 [1,1] 256건, [1,2]·[1,1,1,2] 768건 — 속도 차이가 생기면 늘어난다
    let m = |k: &str, x: usize| (k.to_string(), x);
    let inter = |id: usize| vec![m("CFin/inter", 0), m("CFid/inter", id), m("CFst/inter", 0)];
    assert_eq!(seen, [inter(0), inter(0), inter(2), inter(2), vec![m("CFid/same-bit", 256)], vec![m("CFid/same-bit", 768)], vec![m("CFid/same-bit", 768)]]);
}

#[test]
fn d_rounds_close_the_cross_chunk_gap() {
    // 기본 세트 뒤 D 회차를 이어 돌면, 위에서 고정한 조각 사이 틈이 메워지는지 (같은 결정적 모형, 켜짐 0·1 둘 다)
    let gap = |speeds: &'static [usize], d: u64, words: usize, faults: Vec<(&'static str, Fault)>| {
        let rows = coverage_of(turns(speeds, false, d), words, faults);
        rows.iter().map(|r| r.2 - r.1).sum::<usize>()
    };
    // 64칸 워드 간 결합: 속도 [1,3]·[1,100] 의 멱등 결합 2건 → D 2회차 뒤 0
    for speeds in [&[1, 3][..], &[1, 100][..]] {
        assert_eq!(gap(speeds, 2, WORDS, inter_only(WORDS)), 0, "{speeds:?}");
    }
    // 128칸 같은 비트 멱등 결합 65,024건: D 2회차 뒤 일꾼 둘은 0, [1,1,1,2] 는 120건 남고, 4회차 뒤에는 시험한 속도 모두 0
    let two: Vec<usize> = [&[1, 1][..], &[1, 2][..], &[1, 1, 1, 2][..]].into_iter().map(|sp| gap(sp, 2, 128, same_bit_cfid(128))).collect();
    eprintln!("D 2회차 뒤 같은 비트 결합 놓친 수 [1,1]/[1,2]/[1,1,1,2]: {two:?}");
    assert_eq!(two, [0, 0, 120]);
    for speeds in [&[1, 1, 1, 2][..], &[1, 2, 3, 4][..], &[1, 2, 1][..], &[1, 1, 1, 5][..], &[2, 1][..]] {
        assert_eq!(gap(speeds, 4, 128, same_bit_cfid(128)), 0, "D 4회차 뒤 {speeds:?}");
    }
    eprintln!("D 4회차 뒤 같은 비트 결합: 속도 [1,1,1,2]·[1,2,3,4]·[1,2,1]·[1,1,1,5]·[2,1] 모두 놓친 것 0 (이 모형·목록 기준)");
}

#[test]
fn old_pass_gaps() {
    let one = coverage(old(1), WORDS);
    print_table("비교: 0.4.0 패스 1회전(6무늬, 워드당 24번 접근):", &one);
    let two = coverage(old(2), WORDS);
    print_table("비교: 0.4.0 패스 2회전(12무늬, 워드당 48번 접근):", &two);
    assert_eq!(one.iter().map(|r| r.0).collect::<Vec<_>>(), KINDS);
    // 실측(이 목록·정의 기준): 멱등 결합(워드 간·워드 안)과 워드 안 상태 결합은 2회전에도 100% 미만, 데이터선 단락은 1회전에 4건을 놓친다
    for (kind, rows) in [("1회전", &one), ("2회전", &two)] {
        for &(k, c, t) in rows.iter() {
            let gap = ["CFid/inter", "CFid/intra", "CFst/intra"].contains(&k) || (k == "LineShort" && kind == "1회전");
            assert_eq!(c < t, gap, "{kind} {k}: {c}/{t}");
        }
    }
    assert_eq!(rate(&one, "CFid/inter"), (14_212, 16_128));
    assert_eq!(rate(&one, "LineShort"), (261_628, 261_632));
}

#[test]
fn forward_idempotent_coupling_needs_a_down_element() {
    // 오름차순만 있으면: 앞 칸(가해)이 0→1 로 바뀌는 순간 뒤 칸(피해)은 아직 옛 값이라 0 으로 강제해도 티가 안 난다
    let solid = |c: &mut dyn Cells| (0..4).any(|pass| old_pass(c, pass));
    let forward = Fault::CfId { agg: (3, 9), vic: (40, 9), rising: true, force: false };
    let backward = Fault::CfId { agg: (40, 9), vic: (3, 9), rising: true, force: false };
    for init in [0, u64::MAX] {
        assert!(!solid(&mut SimMem::new(WORDS, init, Some(forward))), "앞→뒤 멱등 결합이 고정 무늬로 보였다");
        assert!(solid(&mut SimMem::new(WORDS, init, Some(backward))), "뒤→앞 은 고정 무늬로 보여야 한다");
        // March C- 의 내림차순 원소에서 잡힌다
        let b = &base_set()[1].1;
        let mut m = SimMem::new(WORDS, init, Some(forward));
        let first = b.iter().position(|el| run_element(&mut m, 0, el).is_err());
        assert_eq!(first.map(|e| b[e - 1].order), Some(Order::Down), "내림차순 원소 다음 읽기에서 드러나야 한다");
    }
}

/// step 으로만 도는 기준 실행기 (빠른 루프와 비교용)
fn by_step(c: &mut dyn Cells, base: usize, el: &Element) -> Result<(), Miss> {
    let n = c.len();
    let mut done = 0;
    for line in lines(el.order, el.walk, n.div_ceil(LINE_WORDS)) {
        step(c, base, el, line, done)?;
        done += LINE_WORDS.min(n - line * LINE_WORDS);
    }
    Ok(())
}

#[test]
fn run_element_matches_step() {
    // 기본 세트 원소 + 기본 세트에 없는 모양·배경(무작위, 쓰고 읽기, 두 배경)
    let mut els: Vec<Element> = base_set().into_iter().flat_map(|s| s.1).collect();
    els.extend((0..2).flat_map(d_round));
    els.push(Element { order: Order::Down, walk: Walk::Stride, ops: vec![Op::R(Bg::Random(5), true), Op::W(Bg::Random(5), false)] });
    els.push(Element { order: Order::Up, walk: Walk::Linear, ops: vec![Op::W(Bg::Random(5), false), Op::R(Bg::Random(5), false)] });
    els.push(Element { order: Order::Down, walk: Walk::Stride, ops: vec![Op::R(Bg::Hash, false), Op::W(Bg::Stripe(7), true)] });
    let faults: Vec<Option<Fault>> = std::iter::once(None).chain(fault_catalog(WORDS).into_iter().step_by(97).map(|f| Some(f.1))).collect();
    for f in &faults {
        for base in [0, 13] {
            let (mut a, mut b) = (SimMem::new(WORDS, 0, *f), SimMem::new(WORDS, 0, *f));
            for el in &els {
                assert_eq!(run_element(&mut a, base, el), by_step(&mut b, base, el), "{f:?} {el:?}");
                assert_eq!(a.cells(), b.cells(), "{f:?} {el:?}");
            }
        }
    }
}

#[test]
fn clean_sim_passes_base_set() {
    for init in [0, u64::MAX] {
        assert!(!base(&mut SimMem::new(WORDS, init, None)), "고장 없는 메모리에서 오류");
        assert!(!turns(&[1, 2, 1, 1], false, 3)(&mut SimMem::new(WORDS, init, None)));
        assert!(!turns(&[3, 1, 2], true, 2)(&mut SimMem::new(WORDS + 5, init, None)), "나머지 칸이 있는 조각");
    }
}

#[test]
fn run_element_reports_place() {
    let b = &base_set()[1].1;
    // 칸 5 비트 3 이 1 에 고착: ⇕(w0) 뒤 ⇑(r0,w1) 에서 칸 5 가 1<<3 로 읽힌다 (앞 5칸을 끝낸 뒤)
    let mut m = SimMem::new(WORDS, 0, Some(Fault::Saf { word: 5, bit: 3, val: true }));
    assert!(run_element(&mut m, 0, &b[0]).is_ok());
    let miss = run_element(&mut m, 0, &b[1]).unwrap_err();
    assert_eq!((miss.i, miss.done, miss.want, miss.got), (5, 5, 0, 1 << 3));
    // 내림차순에서는 끝에서부터 센다: 칸 60 고착 0 → ⇓(r1,w0) 에서 칸 63..61 을 끝낸 뒤
    let mut m = SimMem::new(WORDS, 0, Some(Fault::Saf { word: 60, bit: 0, val: false }));
    let first = b.iter().find_map(|el| run_element(&mut m, 0, el).err()).unwrap();
    assert_eq!((first.i, first.done, first.want, first.got), (60, 60, u64::MAX, u64::MAX - 1));
    // base 는 배경의 칸 번호에 더해진다 (주소고유값)
    let a = &base_set()[0].1[0];
    let (mut x, mut y) = (SimMem::new(8, 0, None), SimMem::new(16, 0, None));
    run_element(&mut x, 8, a).unwrap();
    run_element(&mut y, 0, a).unwrap();
    assert_eq!(x.cells(), &y.cells()[8..]);
}

#[test]
fn sim_without_fault_is_plain_memory() {
    let mut m = SimMem::new(16, 7, None);
    let mut plain = vec![7u64; 16];
    let mut x = 1u64;
    for step in 0..1000u64 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let i = (x >> 40) as usize % 16;
        if step % 3 == 0 {
            assert_eq!(m.read(i), plain[i]);
        } else {
            m.write(i, x);
            plain[i] = x;
        }
    }
    assert_eq!(m.cells(), &plain[..]);
    assert_eq!(m.len(), 16);
}

#[test]
fn saf_holds_bit() {
    let mut m = SimMem::new(4, 0, Some(Fault::Saf { word: 2, bit: 9, val: true }));
    assert_eq!(m.read(2), 1 << 9, "켜질 때부터 고착");
    m.write(2, 0);
    assert_eq!(m.read(2), 1 << 9);
    m.write(1, 0);
    assert_eq!(m.read(1), 0, "다른 칸은 멀쩡");
    let mut m = SimMem::new(4, u64::MAX, Some(Fault::Saf { word: 2, bit: 9, val: false }));
    m.write(2, u64::MAX);
    assert_eq!(m.read(2), !(1 << 9));
}

#[test]
fn tf_blocks_one_direction() {
    // 0→1 이 안 되는 칸: 1→0 은 된다
    let mut m = SimMem::new(4, 0, Some(Fault::Tf { word: 1, bit: 4, rising: true }));
    m.write(1, 1 << 4);
    assert_eq!(m.read(1), 0);
    let mut m = SimMem::new(4, u64::MAX, Some(Fault::Tf { word: 1, bit: 4, rising: true }));
    m.write(1, 0);
    assert_eq!(m.read(1), 0, "1→0 은 된다");
    m.write(1, 1 << 4);
    assert_eq!(m.read(1), 0, "다시 0→1 은 안 된다");
    // 1→0 이 안 되는 칸
    let mut m = SimMem::new(4, u64::MAX, Some(Fault::Tf { word: 1, bit: 4, rising: false }));
    m.write(1, 0);
    assert_eq!(m.read(1), 1 << 4);
    m.write(1, 0b1);
    assert_eq!(m.read(1), 0b1_0001, "1→0 은 여전히 막히고, 다른 비트는 써진다");
}

#[test]
fn af_alias_and_none() {
    let mut m = SimMem::new(4, 0, Some(Fault::AfAlias { a: 1, b: 3 }));
    m.write(1, 11);
    assert_eq!(m.cells(), &[0, 0, 0, 11], "주소 1 쓰기가 칸 3 으로 간다");
    m.write(3, 33);
    assert_eq!(m.read(1), 33, "주소 1 읽기도 칸 3");
    let mut m = SimMem::new(4, 5, Some(Fault::AfNone { a: 2 }));
    m.write(2, 9);
    assert_eq!((m.read(2), m.cells()[2]), (0, 5), "쓰기는 사라지고 읽으면 0");
}

#[test]
fn cf_in_inverts_on_transition() {
    let f = Fault::CfIn { agg: (0, 1), vic: (2, 5), rising: true };
    let mut m = SimMem::new(4, 0, Some(f));
    m.write(0, 0b10);
    assert_eq!(m.read(2), 1 << 5, "0→1 에 뒤집힘");
    m.write(0, 0b10);
    assert_eq!(m.read(2), 1 << 5, "1→1 은 그대로");
    m.write(0, 0);
    assert_eq!(m.read(2), 1 << 5, "1→0 은 그대로");
    m.write(0, 0b10);
    assert_eq!(m.read(2), 0, "다시 0→1 에 뒤집힘");
}

#[test]
fn cf_id_forces_on_transition() {
    let f = Fault::CfId { agg: (3, 0), vic: (1, 0), rising: false, force: true };
    let mut m = SimMem::new(4, u64::MAX, Some(f));
    m.write(1, 0);
    m.write(3, 1);
    assert_eq!(m.read(1), 0, "1→1 은 아무 일 없음");
    m.write(3, 0);
    assert_eq!(m.read(1), 1, "1→0 에 1 로 강제");
    m.write(1, 0);
    m.write(3, 0);
    assert_eq!(m.read(1), 0, "0→0 은 아무 일 없음");
    // 같은 칸 안 비트끼리
    let f = Fault::CfId { agg: (2, 7), vic: (2, 8), rising: true, force: false };
    let mut m = SimMem::new(4, 0, Some(f));
    m.write(2, (1 << 7) | (1 << 8));
    assert_eq!(m.read(2), 1 << 7);
}

#[test]
fn cf_st_holds_while_aggressor_in_state() {
    let f = Fault::CfSt { agg: (0, 2), vic: (1, 3), when: true, force: false };
    let mut m = SimMem::new(4, 0, Some(f));
    m.write(1, 1 << 3);
    assert_eq!(m.read(1), 1 << 3, "가해 비트가 0 이면 멀쩡");
    m.write(0, 1 << 2);
    assert_eq!(m.read(1), 0, "가해 비트가 1 이 되면 0 으로 묶임");
    m.write(1, 1 << 3);
    assert_eq!(m.read(1), 0, "1 인 동안은 써도 0");
    m.write(0, 0);
    m.write(1, 1 << 3);
    assert_eq!(m.read(1), 1 << 3, "풀리면 다시 써진다");
}

#[test]
fn line_write_lets_coupling_win() {
    // 칸 0 비트 1 이 0→1 이면 칸 2 비트 5 를 0 으로 — 같은 줄 쓰기에서 피해 칸에 1 을 함께 써도 결합이 이긴다
    let f = Fault::CfId { agg: (0, 1), vic: (2, 5), rising: true, force: false };
    let mut m = SimMem::new(16, 0, Some(f));
    m.write_line(0, &[0b10, 0, 1 << 5]);
    assert_eq!(m.cells()[..3], [0b10, 0, 0]);
    // 칸마다 따로 쓰면 나중 쓰기가 덮는다
    let mut m = SimMem::new(16, 0, Some(f));
    for (i, v) in [0b10, 0, 1 << 5].into_iter().enumerate() {
        m.write(i, v);
    }
    assert_eq!(m.cells()[..3], [0b10, 0, 1 << 5]);
    // 주소 고장·전이 고장은 줄째 쓰기에서도 칸마다 걸린다
    let mut m = SimMem::new(16, 0, Some(Fault::AfAlias { a: 9, b: 12 }));
    m.write_line(8, &[1, 2, 3]);
    assert_eq!(m.cells()[8..13], [1, 0, 3, 0, 2]);
    let mut m = SimMem::new(16, 0, Some(Fault::Tf { word: 9, bit: 0, rising: true }));
    m.write_line(8, &[1, 1]);
    assert_eq!(m.cells()[8..10], [1, 0]);
}

#[test]
fn line_short_joins_two_line_positions_in_every_line() {
    // q1 = 칸 0 비트 1, q2 = 칸 2 비트 3 (줄 안 위치 64·2 + 3)
    let f = Fault::LineShort { q1: 1, q2: 131, and: true };
    let mut m = SimMem::new(16, 0, Some(f));
    m.write(2, 1 << 3);
    assert_eq!(m.read(2), 0, "AND: 다른 자리가 0 이라 0 으로 읽힌다");
    assert_eq!(m.cells()[2], 1 << 3, "저장된 값은 멀쩡");
    m.write(0, 0b10);
    assert_eq!((m.read(0), m.read(2)), (0b10, 1 << 3), "둘 다 1 이면 그대로");
    m.write(0, 0);
    assert_eq!((m.read(0), m.read(2)), (0, 0), "다른 칸 자리의 값에 따라 읽기가 바뀐다");
    m.write(1, 0b10);
    assert_eq!(m.read(1), 0b10, "두 자리가 없는 칸은 멀쩡");
    // 두 번째 줄(칸 8~15)도 같은 자리가 붙어 있다
    m.write(10, 1 << 3);
    assert_eq!(m.read(10), 0);
    let f = Fault::LineShort { q1: 1, q2: 131, and: false };
    let mut m = SimMem::new(16, 0, Some(f));
    m.write(8, 0b10);
    assert_eq!((m.read(8), m.read(10)), (0b10, 1 << 3), "OR: 1 이 번진다");
    assert_eq!((m.read(0), m.read(2)), (0, 0), "다른 줄은 멀쩡");
}

#[test]
fn catalog_sizes_and_pair_kinds() {
    let cat = fault_catalog(WORDS);
    let count = |kind: &str| cat.iter().filter(|c| c.0 == kind).count();
    let bits = WORDS * 64;
    assert_eq!((count("SAF"), count("TF")), (bits * 2, bits * 2));
    assert_eq!((count("AF-alias"), count("AF-none")), (WORDS * (WORDS - 1), WORDS));
    // 결합: 워드 쌍(워드 간) / 비트 쌍(워드 안) 전부 × 방향
    let pairs = WORDS * (WORDS - 1);
    assert_eq!((count("CFin/inter"), count("CFid/inter"), count("CFst/inter")), (pairs * 2, pairs * 4, pairs * 4));
    assert_eq!((count("CFin/intra"), count("CFid/intra"), count("CFst/intra")), (64 * 63 * 2, 64 * 63 * 4, 64 * 63 * 4));
    assert_eq!(count("LineShort"), 512 * 511 / 2 * 2);
    for (kind, f) in &cat {
        match *f {
            Fault::CfIn { agg, vic, .. } | Fault::CfId { agg, vic, .. } | Fault::CfSt { agg, vic, .. } => {
                assert_ne!(agg, vic);
                assert_eq!(kind.ends_with("intra"), agg.0 == vic.0, "{kind} {f:?}");
                assert!(agg.0 < WORDS && vic.0 < WORDS && agg.1 < 64 && vic.1 < 64);
            }
            Fault::LineShort { q1, q2, .. } => assert!(q1 < q2 && q2 < 512),
            _ => {}
        }
    }
}
