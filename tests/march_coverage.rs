//! 고장 모형 시뮬레이터: 실제 검사 코드(sweep_pass)를 고장 하나 품은 작은 가짜 메모리에 돌려 고장 종류별 검출률을 잰다.
//! 지금 방식(오름차순만 있는 3단계 패스 × 6무늬)의 빈틈을 숫자로 단언한다 — 새 순서를 넣는 작업에서 100% 로 바꾼다.

use steadycheck::mem::{sweep_pass, Cells, PassEnd};
use steadycheck::memsim::{coverage, fault_catalog, Fault, SimMem};

/// 시뮬레이터 메모리: 64칸 = 캐시 줄 8개
const WORDS: usize = 64;

/// 지금 방식 rounds 회전(회전마다 6무늬)을 돌려 어긋남이 나오면 true
fn current(rounds: u64) -> impl Fn(&mut dyn Cells) -> bool {
    move |c| (0..6 * rounds).any(|pass| matches!(sweep_pass(c, 0, pass, |_| true), PassEnd::Mismatch { .. }))
}

fn rate(rows: &[(&str, usize, usize)], kind: &str) -> (usize, usize) {
    let r = rows.iter().find(|r| r.0 == kind).unwrap_or_else(|| panic!("{kind} 없음"));
    (r.1, r.2)
}

fn print_table(title: &str, rows: &[(&str, usize, usize)]) {
    eprintln!("{title}");
    for (kind, caught, total) in rows {
        eprintln!("  {kind:<11} {caught:>7}/{total:<7} {:>7.2}%", 100.0 * *caught as f64 / *total as f64);
    }
}

#[test]
fn current_pass_gaps() {
    let one = coverage(current(1), WORDS);
    print_table("지금 방식 1회전(6무늬, 워드당 24번 접근):", &one);
    let two = coverage(current(2), WORDS);
    print_table("지금 방식 2회전(12무늬, 워드당 48번 접근):", &two);
    let kinds: Vec<_> = one.iter().map(|r| r.0).collect();
    assert_eq!(kinds, ["SAF", "TF", "AF-alias", "AF-none", "CFin/inter", "CFin/intra", "CFid/inter", "CFid/intra", "CFst/inter", "CFst/intra", "LineShort"]);
    // 실측(64칸, 켜짐 0·1 둘 다): 단순 고장·반전 결합·워드 간 상태 결합은 1회전에 전부 잡는다.
    // 빈틈: 멱등 결합(워드 간·워드 안)과 워드 안 상태 결합은 2회전에도 100% 미만 — 무작위·주소 무늬의 운으로만 메워진다.
    // 데이터선 단락은 1회전에 4건(같은 칸 안 같은 홀짝 비트 두 쌍 × AND/OR)을 놓치고 2회전에 다 잡는다
    for (kind, rows) in [("1회전", &one), ("2회전", &two)] {
        for &(k, c, t) in rows.iter() {
            let gap = ["CFid/inter", "CFid/intra", "CFst/intra"].contains(&k) || (k == "LineShort" && kind == "1회전");
            assert_eq!(c < t, gap, "{kind} {k}: {c}/{t}");
        }
    }
    assert_eq!(rate(&one, "LineShort"), (261_628, 261_632));
    // 회전을 늘리면 운이 더 붙지만 닫히지는 않는다
    for k in ["CFid/inter", "CFid/intra", "CFst/intra"] {
        assert!(rate(&two, k).0 > rate(&one, k).0, "{k}: 2회전이 더 잡아야 한다");
    }
}

#[test]
fn solid_patterns_never_see_forward_idempotent_coupling() {
    // 오름차순만 있으면: 앞 칸(가해)이 0→1 로 바뀌는 순간 뒤 칸(피해)은 아직 옛 값이라 0 으로 강제해도 티가 안 난다
    let solid = |c: &mut dyn Cells| (0..4).any(|pass| matches!(sweep_pass(c, 0, pass, |_| true), PassEnd::Mismatch { .. }));
    let forward = Fault::CfId { agg: (3, 9), vic: (40, 9), rising: true, force: false };
    let backward = Fault::CfId { agg: (40, 9), vic: (3, 9), rising: true, force: false };
    for init in [0, u64::MAX] {
        assert!(!solid(&mut SimMem::new(WORDS, init, Some(forward))), "앞→뒤 멱등 결합이 고정 무늬로 보였다");
        assert!(solid(&mut SimMem::new(WORDS, init, Some(backward))), "뒤→앞 은 고정 무늬로 보여야 한다");
    }
}

#[test]
fn clean_sim_passes_every_pattern() {
    for init in [0, u64::MAX] {
        let mut m = SimMem::new(WORDS, init, None);
        for pass in 0..12 {
            assert_eq!(sweep_pass(&mut m, 0, pass, |_| true), PassEnd::Clean, "init={init:#x} pass={pass}");
        }
        assert!(!current(2)(&mut SimMem::new(WORDS, init, None)), "고장 없는 메모리에서 오류");
    }
}

#[test]
fn sweep_pass_reports_place_and_stage() {
    // 칸 5 비트 3 이 1 에 고착: 패스 2(전부 0) 의 2단계에서 칸 5 가 1<<3 로 읽힌다
    let mut m = SimMem::new(WORDS, 0, Some(Fault::Saf { word: 5, bit: 3, val: true }));
    assert_eq!(sweep_pass(&mut m, 0, 2, |_| true), PassEnd::Mismatch { i: 5, want: 0, got: 1 << 3, complement: false });
    // 0 에 고착: 패스 2 의 3단계(뒤집은 값 = 전부 1)에서 걸린다
    let mut m = SimMem::new(WORDS, 0, Some(Fault::Saf { word: 5, bit: 3, val: false }));
    assert_eq!(sweep_pass(&mut m, 0, 2, |_| true), PassEnd::Mismatch { i: 5, want: u64::MAX, got: !(1 << 3), complement: true });
    // 단계 사이에서 멈추라고 하면 멈추고, 불린 순서는 1단계 뒤(false) → 2단계 뒤(true)
    let mut seen = Vec::new();
    assert_eq!(sweep_pass(&mut SimMem::new(WORDS, 0, None), 0, 0, |late| { seen.push(late); !late }), PassEnd::Stopped);
    assert_eq!(seen, [false, true]);
    // base 는 무늬 값의 칸 번호에 더해진다 (주소 무늬)
    let mut a = SimMem::new(8, 0, None);
    let mut b = SimMem::new(16, 0, None);
    sweep_pass(&mut a, 8, 4, |_| true);
    sweep_pass(&mut b, 0, 4, |_| true);
    assert_eq!(a.cells(), &b.cells()[8..]);
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
    assert_eq!(m.read(1), 0b1_0001, "같은 값 유지(1→1)는 된다");
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
fn catalog_covers_every_place_and_direction() {
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
