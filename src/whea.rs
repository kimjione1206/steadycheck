//! 윈도우 하드웨어 오류 기록(시스템 로그의 WHEA-Logger) 세기 — 판정과 무관한 참고 정보.
//! 메모리 정정 오류(ECC 가 고친 것)처럼 검사가 못 보는 오류가 윈도우에는 남을 수 있다

use std::collections::BTreeMap;

#[derive(Debug, PartialEq, serde::Serialize)]
pub struct Whea {
    /// 검사하는 동안 기록된 WHEA 사건 수 (윈도우 사건 번호 → 수, 없으면 빈 객체)
    pub during_run: BTreeMap<u32, u64>,
    /// 검사 시작 전 7일 동안 기록된 수
    pub before_7_days: u64,
}

/// wevtutil XML 출력에서 사건 번호들
pub fn event_ids(xml: &str) -> Vec<u32> {
    xml.split("<EventID").skip(1).filter_map(|s| s.split_once('>')).filter_map(|(_, rest)| rest.split_once("</EventID>")).filter_map(|(n, _)| n.trim().parse().ok()).collect()
}

pub fn count_by_id(ids: &[u32]) -> BTreeMap<u32, u64> {
    let mut m = BTreeMap::new();
    for &id in ids {
        *m.entry(id).or_insert(0) += 1;
    }
    m
}

/// 지금부터 거꾸로 ms 동안의 WHEA 사건 번호들. 조회가 실패하면 None
#[cfg(windows)]
fn ids_within(ms: u64) -> Option<Vec<u32>> {
    let q = format!("/q:*[System[Provider[@Name='Microsoft-Windows-WHEA-Logger'] and TimeCreated[timediff(@SystemTime) <= {ms}]]]");
    let out = std::process::Command::new("wevtutil").args(["qe", "System", &q, "/f:xml"]).output().ok()?;
    out.status.success().then(|| event_ids(&String::from_utf8_lossy(&out.stdout)))
}

/// run_ms = 검사에 걸린 시간. 검사 중 = 최근 run_ms + 2초, 그 전 7일 = 최근 (7일 + 그것) 에서 검사 중을 뺀 것
#[cfg(windows)]
pub fn query(run_ms: u64) -> Option<Whea> {
    let during = ids_within(run_ms + 2_000)?;
    let week = ids_within(run_ms + 2_000 + 7 * 24 * 3600 * 1000)?;
    Some(Whea { before_7_days: (week.len() - during.len().min(week.len())) as u64, during_run: count_by_id(&during) })
}

#[cfg(not(windows))]
pub fn query(_run_ms: u64) -> Option<Whea> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_event_ids_with_and_without_attributes() {
        let xml = "<Event><System><Provider Name='Microsoft-Windows-WHEA-Logger'/><EventID>19</EventID></System></Event>\
                   <Event><System><EventID Qualifiers='0'>47</EventID></System></Event>\
                   <Event><System><EventID>19</EventID></System></Event>";
        assert_eq!(event_ids(xml), vec![19, 47, 19]);
        let by = count_by_id(&event_ids(xml));
        assert_eq!(by.get(&19), Some(&2));
        assert_eq!(by.get(&47), Some(&1));
    }

    #[test]
    fn parse_garbage_is_empty() {
        assert!(event_ids("").is_empty());
        assert!(event_ids("<EventID>abc</EventID><EventID>").is_empty());
    }

    #[cfg(not(windows))]
    #[test]
    fn query_is_none_off_windows() {
        assert!(query(1000).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn query_works_on_windows() {
        // 서버에는 보통 기록이 없지만, 조회 자체는 성공해야 한다(문법·권한 오류면 None)
        let w = query(60_000).expect("wevtutil 조회 실패");
        assert!(w.during_run.values().all(|&n| n > 0));
    }
}
