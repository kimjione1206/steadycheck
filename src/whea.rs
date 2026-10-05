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

/// 시스템 로그의 WHEA 기록 원본 이름
#[cfg(windows)]
const WHEA_PROVIDER: &str = "Microsoft-Windows-WHEA-Logger";

/// 시스템 폴더의 wevtutil 전체 경로 — 이름만 주면 실행 파일 폴더를 먼저 찾아 같은 이름의 다른 파일이 실행될 수 있다
#[cfg(windows)]
fn wevtutil_path() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").filter(|r| !r.is_empty()).unwrap_or_else(|| r"C:\Windows".into());
    std::path::Path::new(&root).join("System32").join("wevtutil.exe")
}

/// provider 가 시스템 로그에 남긴 사건 중 지금부터 거꾸로 (after_ms, within_ms] 사이의 사건 번호들. 조회가 실패하면 None
#[cfg(windows)]
fn ids_between(provider: &str, after_ms: u64, within_ms: u64) -> Option<Vec<u32>> {
    let q = format!("/q:*[System[Provider[@Name='{provider}'] and TimeCreated[timediff(@SystemTime) > {after_ms} and timediff(@SystemTime) <= {within_ms}]]]");
    let out = std::process::Command::new(wevtutil_path()).args(["qe", "System", &q, "/f:xml"]).output().ok()?;
    out.status.success().then(|| event_ids(&String::from_utf8_lossy(&out.stdout)))
}

/// run_ms = 검사에 걸린 시간. 검사 중 = 최근 run_ms + 2초, 그 전 7일 = 그보다 앞선 7일 (두 창은 겹치지 않는다)
#[cfg(windows)]
pub fn query(run_ms: u64) -> Option<Whea> {
    let during_ms = run_ms + 2_000;
    let during = ids_between(WHEA_PROVIDER, 0, during_ms)?;
    let before = ids_between(WHEA_PROVIDER, during_ms, during_ms + 7 * 24 * 3600 * 1000)?;
    Some(Whea { during_run: count_by_id(&during), before_7_days: before.len() as u64 })
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
    fn wevtutil_path_is_system_copy() {
        let p = wevtutil_path();
        assert!(p.is_absolute() && p.ends_with(r"System32\wevtutil.exe"), "{p:?}");
        assert!(p.exists(), "{p:?}");
    }

    #[cfg(windows)]
    #[test]
    fn query_works_on_windows() {
        // 서버에는 보통 기록이 없지만, 조회 자체는 성공해야 한다(문법·권한 오류면 None)
        query(60_000).expect("wevtutil 조회 실패");
    }

    #[cfg(windows)]
    #[test]
    fn query_filter_matches_real_entries() {
        // 서버에는 WHEA 기록이 없으므로, 다른 원본 이름으로 시스템 로그에 1건 남겨 원본·시간 조건과 실제 출력 읽기를 확인한다(러너는 관리자 권한)
        let src = "steadycheck-whea-test";
        let st = std::process::Command::new("eventcreate")
            .args(["/T", "INFORMATION", "/ID", "999", "/L", "SYSTEM", "/SO", src, "/D", "steadycheck test"])
            .status()
            .unwrap();
        assert!(st.success(), "eventcreate 실패: {st}");
        let recent = ids_between(src, 0, 60_000).expect("조회 실패");
        assert!(recent.contains(&999), "원본·시간 조건이나 출력 읽기가 실제 기록을 못 맞춤: {recent:?}");
        std::thread::sleep(std::time::Duration::from_secs(3));
        let latest = ids_between(src, 0, 1_000).expect("조회 실패");
        assert!(!latest.contains(&999), "시간 창이 지난 기록을 포함함: {latest:?}");
        let earlier = ids_between(src, 1_000, 60_000).expect("조회 실패");
        assert!(earlier.contains(&999), "앞선 창(아래 경계 있음)이 기록을 못 맞춤: {earlier:?}");
    }
}
