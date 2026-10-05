//! 윈도우 하드웨어 오류 기록(시스템 로그의 WHEA-Logger) 세기 — 판정과 무관한 참고 정보.
//! 메모리 정정 오류(ECC 가 고친 것)처럼 검사가 못 보는 오류가 윈도우에는 남을 수 있다

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
