//! 시험 전용: 고장 하나를 품은 작은 가짜 메모리. 실제 검사 코드를 Cells 창구로 그대로 돌려 고장 종류별 검출률을 잰다.
//! 실행 경로에서는 쓰지 않는다. 칸 = 64비트 워드, 캐시 줄 = 칸 8개, 줄 안 비트 위치 q = 64·(칸 % 8) + 비트.

use crate::kernel::splitmix64;
use crate::mem::{Cells, LINE_WORDS};

/// 메모리 고장 하나. 비트 위치는 (칸, 비트)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fault {
    /// 고착: 칸 word 의 비트 bit 가 늘 val
    Saf { word: usize, bit: u32, val: bool },
    /// 전이: rising 이면 0→1, 아니면 1→0 으로 바뀌지 못한다
    Tf { word: usize, bit: u32, rising: bool },
    /// 주소 디코더: 주소 a 가 칸 b 를 잡는다 (칸 a 는 닿지 않고, 칸 b 는 주소가 둘)
    AfAlias { a: usize, b: usize },
    /// 주소 디코더: 주소 a 가 아무 칸도 잡지 않는다 — 쓰기는 사라지고 읽으면 0
    AfNone { a: usize },
    /// 반전 결합: 가해 비트가 rising(0→1)/아니면(1→0) 방향으로 바뀌면 피해 비트가 뒤집힌다
    CfIn { agg: (usize, u32), vic: (usize, u32), rising: bool },
    /// 멱등 결합: 가해 비트가 그 방향으로 바뀌면 피해 비트가 force 가 된다
    CfId { agg: (usize, u32), vic: (usize, u32), rising: bool, force: bool },
    /// 상태 결합: 가해 비트가 when 인 동안 피해 비트가 force 로 묶인다
    CfSt { agg: (usize, u32), vic: (usize, u32), when: bool, force: bool },
    /// 데이터선 단락: 모든 캐시 줄에서 줄 안 위치 q1·q2 가 붙어, 줄을 읽어 오면 두 자리가 AND(and)/OR 로 합쳐져 나온다.
    /// 저장된 값은 멀쩡하다 (캐시가 맞는 값을 들고 써 보내는 것과 같은 결과)
    LineShort { q1: u32, q2: u32, and: bool },
}

/// 정상 칸들 + 고장 하나
pub struct SimMem {
    cells: Vec<u64>,
    fault: Option<Fault>,
}

fn get(cells: &[u64], (w, b): (usize, u32)) -> bool {
    cells[w] >> b & 1 == 1
}

fn set(cells: &mut [u64], (w, b): (usize, u32), v: bool) {
    cells[w] = (cells[w] & !(1 << b)) | ((v as u64) << b);
}

/// 줄 안 위치 q 를 줄 line 의 (칸, 비트)로
fn at(line: usize, q: u32) -> (usize, u32) {
    (line * LINE_WORDS + q as usize / 64, q % 64)
}

impl SimMem {
    /// words 칸을 모두 init 으로 켠다 (고착·상태 결합은 켜질 때부터 걸려 있다)
    pub fn new(words: usize, init: u64, fault: Option<Fault>) -> SimMem {
        let mut m = SimMem { cells: vec![init; words], fault };
        m.enforce();
        m
    }

    /// 칸에 실제로 저장된 값 (시험에서 들여다보기용)
    pub fn cells(&self) -> &[u64] {
        &self.cells
    }

    /// 상태로 정해지는 고장(고착·상태 결합)을 매 조작 뒤 다시 건다
    fn enforce(&mut self) {
        match self.fault {
            Some(Fault::Saf { word, bit, val }) => set(&mut self.cells, (word, bit), val),
            Some(Fault::CfSt { agg, vic, when, force }) if get(&self.cells, agg) == when => set(&mut self.cells, vic, force),
            _ => {}
        }
    }
}

impl Cells for SimMem {
    fn len(&self) -> usize {
        self.cells.len()
    }

    fn read(&mut self, i: usize) -> u64 {
        // 읽기는 칸 상태를 바꾸지 않으므로 다시 걸 고장이 없다
        match self.fault {
            Some(Fault::AfAlias { a, b }) if i == a => self.cells[b],
            Some(Fault::AfNone { a }) if i == a => 0,
            Some(Fault::LineShort { q1, q2, and }) => {
                let line = i / LINE_WORDS;
                let (p1, p2) = (at(line, q1), at(line, q2));
                let mut v = self.cells[i];
                if p1.0.max(p2.0) < self.cells.len() && (p1.0 == i || p2.0 == i) {
                    let (b1, b2) = (get(&self.cells, p1), get(&self.cells, p2));
                    let joined = if and { b1 & b2 } else { b1 | b2 };
                    for p in [p1, p2].into_iter().filter(|p| p.0 == i) {
                        v = (v & !(1 << p.1)) | ((joined as u64) << p.1);
                    }
                }
                v
            }
            _ => self.cells[i],
        }
    }

    fn write(&mut self, i: usize, v: u64) {
        self.write_line(i, &[v]);
    }

    /// 줄째 쓰기: 칸마다 저장(주소 변환·전이 거르기)을 모두 한 뒤 결합 효과를 건다 —
    /// 같은 순간에 쓰인 칸끼리는 결합 효과가 그 쓰기를 이긴다(한 칸 안 비트끼리와 같은 규칙)
    fn write_line(&mut self, i: usize, v: &[u64]) {
        let mut stored = [(0usize, 0u64, 0u64); LINE_WORDS];
        let mut count = 0;
        for (j, &x) in v.iter().enumerate() {
            let target = match self.fault {
                Some(Fault::AfAlias { a, b }) if i + j == a => Some(b),
                Some(Fault::AfNone { a }) if i + j == a => None,
                _ => Some(i + j),
            };
            if let Some(p) = target {
                let old = self.cells[p];
                let mut new = x;
                // 저장 거르기: 전이 고장은 막힌 방향의 바뀜을 되돌린다
                if let Some(Fault::Tf { word, bit, rising }) = self.fault {
                    let from = !rising;
                    if p == word && (old >> bit & 1 == 1) == from && (new >> bit & 1 == 1) != from {
                        new = (new & !(1 << bit)) | ((from as u64) << bit);
                    }
                }
                self.cells[p] = new;
                stored[count] = (p, old, new);
                count += 1;
            }
        }
        // 쓴 뒤 효과: 가해 비트가 정한 방향으로 바뀌었으면 피해 비트를 건드린다
        for &(p, old, new) in &stored[..count] {
            let turned = |agg: (usize, u32), rising: bool| {
                let (o, n) = (old >> agg.1 & 1 == 1, new >> agg.1 & 1 == 1);
                p == agg.0 && o != n && n == rising
            };
            match self.fault {
                Some(Fault::CfIn { agg, vic, rising }) if turned(agg, rising) => self.cells[vic.0] ^= 1 << vic.1,
                Some(Fault::CfId { agg, vic, rising, force }) if turned(agg, rising) => set(&mut self.cells, vic, force),
                _ => {}
            }
        }
        self.enforce();
    }
}

/// 작은 메모리(words 칸)의 고장 목록: (종류, 고장). 같은 종류는 이어서 나온다.
/// 단일 칸·주소 고장은 모든 위치·방향, 워드 간 결합은 모든 워드 쌍(비트는 쌍마다 고정 규칙으로 하나), 워드 안 결합은 모든 비트 쌍(칸은 쌍마다 하나),
/// 데이터선 단락은 줄 안 512자리의 모든 쌍 × AND/OR
pub fn fault_catalog(words: usize) -> Vec<(&'static str, Fault)> {
    let mut out = Vec::new();
    for word in 0..words {
        for bit in 0..64 {
            for val in [false, true] {
                out.push(("SAF", Fault::Saf { word, bit, val }));
            }
        }
    }
    for word in 0..words {
        for bit in 0..64 {
            for rising in [false, true] {
                out.push(("TF", Fault::Tf { word, bit, rising }));
            }
        }
    }
    let word_pairs: Vec<(usize, usize)> = (0..words).flat_map(|a| (0..words).filter(move |&b| b != a).map(move |b| (a, b))).collect();
    for &(a, b) in &word_pairs {
        out.push(("AF-alias", Fault::AfAlias { a, b }));
    }
    for a in 0..words {
        out.push(("AF-none", Fault::AfNone { a }));
    }
    // 워드 간: (가해 칸, 피해 칸) 전부, 비트는 쌍 번호를 섞어 고른다
    let inter: Vec<((usize, u32), (usize, u32))> = word_pairs
        .iter()
        .enumerate()
        .map(|(k, &(a, v))| {
            let h = splitmix64(k as u64);
            ((a, (h & 63) as u32), (v, (h >> 6 & 63) as u32))
        })
        .collect();
    // 워드 안: (가해 비트, 피해 비트) 전부, 칸은 쌍 번호를 섞어 고른다
    let intra: Vec<((usize, u32), (usize, u32))> = (0..64u32)
        .flat_map(|x| (0..64u32).filter(move |&y| y != x).map(move |y| (x, y)))
        .enumerate()
        .map(|(k, (x, y))| {
            let w = (splitmix64(k as u64) % words as u64) as usize;
            ((w, x), (w, y))
        })
        .collect();
    for (kind, pairs) in [("CFin/inter", &inter), ("CFin/intra", &intra)] {
        for &(agg, vic) in pairs {
            for rising in [false, true] {
                out.push((kind, Fault::CfIn { agg, vic, rising }));
            }
        }
    }
    for (kind, pairs) in [("CFid/inter", &inter), ("CFid/intra", &intra)] {
        for &(agg, vic) in pairs {
            for rising in [false, true] {
                for force in [false, true] {
                    out.push((kind, Fault::CfId { agg, vic, rising, force }));
                }
            }
        }
    }
    for (kind, pairs) in [("CFst/inter", &inter), ("CFst/intra", &intra)] {
        for &(agg, vic) in pairs {
            for when in [false, true] {
                for force in [false, true] {
                    out.push((kind, Fault::CfSt { agg, vic, when, force }));
                }
            }
        }
    }
    for q1 in 0..512u32 {
        for q2 in q1 + 1..512 {
            for and in [false, true] {
                out.push(("LineShort", Fault::LineShort { q1, q2, and }));
            }
        }
    }
    out
}

/// 비트 축 전수 묶음: 주어진 (가해 칸, 피해 칸) 쌍마다 비트 쌍 64 × 64 전부 × 결합 종류·방향 전부
pub fn coupling_bundle(word_pairs: &[(usize, usize)]) -> Vec<(&'static str, Fault)> {
    let mut out = Vec::new();
    for &(a, v) in word_pairs {
        for ab in 0..64 {
            for vb in 0..64 {
                let (agg, vic) = ((a, ab), (v, vb));
                for rising in [false, true] {
                    out.push(("CFin/all-bits", Fault::CfIn { agg, vic, rising }));
                    for force in [false, true] {
                        out.push(("CFid/all-bits", Fault::CfId { agg, vic, rising, force }));
                        out.push(("CFst/all-bits", Fault::CfSt { agg, vic, when: rising, force }));
                    }
                }
            }
        }
    }
    out
}

/// 검사 run 을 fault_catalog(words) 의 고장마다 돌려 종류별 (종류, 잡음, 전체)를 센다
pub fn coverage(run: impl Fn(&mut dyn Cells) -> bool, words: usize) -> Vec<(&'static str, usize, usize)> {
    coverage_of(run, words, fault_catalog(words))
}

/// 검사 run 을 faults 의 고장마다 돌려 종류별 (종류, 잡음, 전체)를 센다 (처음 나온 순서).
/// 켜질 때 내용은 알 수 없으므로 전부 0·전부 1 로 켠 두 경우 모두 잡아야 "잡음"으로 친다
pub fn coverage_of(run: impl Fn(&mut dyn Cells) -> bool, words: usize, faults: Vec<(&'static str, Fault)>) -> Vec<(&'static str, usize, usize)> {
    let mut out: Vec<(&'static str, usize, usize)> = Vec::new();
    for (kind, f) in faults {
        let caught = [0, u64::MAX].into_iter().all(|init| run(&mut SimMem::new(words, init, Some(f))));
        match out.iter_mut().find(|row| row.0 == kind) {
            Some(row) => {
                row.1 += caught as usize;
                row.2 += 1;
            }
            None => out.push((kind, caught as usize, 1)),
        }
    }
    out
}
