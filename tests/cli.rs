use std::process::{Command, Output};
use std::sync::Mutex;

/// 프로그램은 한 번에 하나만 돌린다. 일꾼을 코어마다 고정하고 1~2초 안에 "모든 일꾼이 한 번 이상"을 보므로,
/// 다른 시험의 프로그램(특히 메모리를 거의 다 잡는 --mb auto)과 겹치면 윈도우 러너(4코어)에서 한 코어의 차례가 밀려 우연히 FAIL 이 난다
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn output(args: &[&str]) -> Output {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    Command::new(env!("CARGO_BIN_EXE_steadycheck")).args(args).output().unwrap()
}

fn run(args: &[&str]) -> (i32, serde_json::Value) {
    let out = output(args);
    let json = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (out.status.code().unwrap(), json)
}

#[test]
fn clean_run_passes() {
    let (code, j) = run(&["all", "--seconds", "1", "--mb", "16", "--iters", "4096", "--threads", "2"]);
    assert_eq!(code, 0, "{j}");
    assert_eq!(j["verdict"], "PASS");
    assert_eq!(j["injected"], false);
    assert!(j["cpu"]["blocks"].as_u64().unwrap() > 0);
    assert!(j["mem"]["passes"].as_u64().unwrap() > 0);
    // all = cpu → share → mem
    assert!(j["share"]["min_thread_messages"].as_u64().unwrap() >= 1, "{j}");
}

#[test]
fn injected_cpu_error_fails_with_code_1() {
    let (code, j) = run(&["cpu", "--seconds", "5", "--iters", "4096", "--threads", "2", "--inject-cpu", "0:2"]);
    assert_eq!(code, 1);
    assert_eq!(j["verdict"], "FAIL");
    assert_eq!(j["injected"], true);
    assert_eq!(j["cpu"]["error"]["block"], 2);
}

#[test]
fn injected_mem_error_fails_with_code_1() {
    let (code, j) = run(&["mem", "--seconds", "5", "--mb", "8", "--inject-mem", "1:500"]);
    assert_eq!(code, 1);
    assert_eq!(j["mem"]["error"]["offset_bytes"], 4000);
    assert_eq!(j["mem"]["error"]["thread"], 0);
}

#[test]
fn usage_error_is_code_3() {
    assert_eq!(run(&["nope"]).0, 3);
}

#[test]
fn unsupported_isa_is_code_2() {
    // 어떤 명령어 세트든 미지원인 게 하나라도 있으면 그걸로 확인 (전부 지원하는 기계면 건너뜀)
    let unsupported = ["avx512", "avx2"].into_iter().find(|s| !steadycheck::kernel::Isa::parse(s).unwrap().supported());
    match unsupported {
        Some(isa) => assert_eq!(run(&["cpu", "--seconds", "1", "--isa", isa]).0, 2),
        None => eprintln!("모든 ISA 지원 — 건너뜀"),
    }
}

#[test]
fn every_kernel_and_pulse_pass_with_throughput() {
    for k in ["chain", "wide", "fma", "fma32", "lz", "mix"] {
        let (code, j) = run(&["cpu", "--seconds", "1", "--threads", "2", "--iters", "4096", "--kernel", k]);
        assert_eq!(code, 0, "{k}: {j}");
        assert_eq!(j["cpu"]["kernels"], k);
        assert!(j["cpu"]["lane_iters_per_sec"].as_u64().unwrap() > 0, "{k}");
    }
    let (code, j) = run(&["cpu", "--seconds", "1", "--threads", "2", "--iters", "4096", "--pattern", "pulse"]);
    assert_eq!(code, 0, "{j}");
    assert_eq!(j["cpu"]["pattern"], "pulse");
}

#[test]
fn cycle_pattern_covers_every_thread() {
    let (code, j) = run(&["cpu", "--seconds", "2", "--threads", "2", "--iters", "4096", "--pattern", "cycle"]);
    assert_eq!(code, 0, "{j}");
    assert_eq!(j["cpu"]["pattern"], "cycle");
    assert!(j["cpu"]["min_thread_blocks"].as_u64().unwrap() >= 1);
}

#[test]
fn cycle_too_short_is_usage_error() {
    // 16코어 × 최소 0.5초 = 8초가 필요한데 1초만 주면 불량(1)이 아니라 사용법 오류(3)
    let out = output(&["cpu", "--pattern", "cycle", "--threads", "16", "--seconds", "1"]);
    assert_eq!(out.status.code(), Some(3));
    // 안내 문구의 최소 초: 16 × 0.5초 = 8초
    assert!(String::from_utf8_lossy(&out.stderr).contains("최소 8초"), "{}", String::from_utf8_lossy(&out.stderr));
    // all 도 CPU 를 돌리므로 막는다
    assert_eq!(run(&["all", "--pattern", "cycle", "--threads", "16", "--seconds", "1"]).0, 3);
    // mem 은 CPU 순환을 쓰지 않으므로 막지 않는다
    let (code, j) = run(&["mem", "--pattern", "cycle", "--threads", "16", "--seconds", "1", "--mb", "8"]);
    assert_eq!(code, 0, "{j}");
}

#[test]
fn cycle_exact_minimum_is_accepted() {
    // 경계: 1초 = 2코어 × 0.5초 는 받아들인다
    let (code, j) = run(&["cpu", "--pattern", "cycle", "--threads", "2", "--seconds", "1", "--iters", "4096"]);
    assert_eq!(code, 0, "{j}");
    // 2초 = 4코어 × 0.5초 도 경계 (초 × 1000 을 초 + 1000 으로 잘못 계산하면 여기서 막힌다)
    let (code, j) = run(&["cpu", "--pattern", "cycle", "--threads", "4", "--seconds", "2", "--iters", "4096"]);
    assert_eq!(code, 0, "{j}");
    // 순환이 아니면 짧아도 막지 않는다
    let (code, j) = run(&["cpu", "--threads", "4", "--seconds", "1", "--iters", "4096"]);
    assert_eq!(code, 0, "{j}");
    // cpu 모드는 RAM 검사를 하지 않는다
    assert!(j["mem"].is_null(), "{j}");
    assert_eq!(j["verdict"], "PASS");
}

#[test]
fn mem_uses_requested_workers() {
    let (code, j) = run(&["mem", "--seconds", "1", "--mb", "16", "--threads", "3"]);
    assert_eq!(code, 0, "{j}");
    // 일꾼은 논리 CPU 수를 넘지 않는다
    assert_eq!(j["mem"]["threads"].as_u64().unwrap(), j["logical_cpus"].as_u64().unwrap().min(3));
    assert!(j["mem"]["min_thread_passes"].as_u64().unwrap() >= 1);
    assert!(j["mem"]["verified_bytes_per_sec"].as_u64().unwrap() > 0);
}

#[test]
fn mem_workers_capped_at_logical_cpus() {
    let (code, j) = run(&["mem", "--seconds", "1", "--mb", "8", "--threads", "999"]);
    assert_eq!(code, 0, "{j}");
    assert_eq!(j["mem"]["threads"], j["logical_cpus"]);
}

#[test]
fn share_passes_with_messages() {
    let (code, j) = run(&["share", "--seconds", "1", "--threads", "2"]);
    assert_eq!(code, 0, "{j}");
    assert_eq!(j["mode"], "share");
    assert_eq!(j["verdict"], "PASS");
    assert!(j["share"]["min_thread_messages"].as_u64().unwrap() >= 1, "{j}");
    assert!(j["cpu"].is_null() && j["mem"].is_null(), "{j}");
}

#[test]
fn injected_share_error_fails_with_code_1() {
    let (code, j) = run(&["share", "--seconds", "5", "--threads", "2", "--inject-share", "1:3"]);
    assert_eq!(code, 1, "{j}");
    assert_eq!(j["injected"], true);
    assert_eq!(j["share"]["error"]["cpu"], 1);
    assert_eq!(j["share"]["error"]["seq"], 3);
}

#[test]
fn mem_base_incomplete_warns_but_keeps_pass() {
    // 1GB 를 일꾼 하나로 3초: 기본 세트(칸당 66번)를 못 끝낸다 — 경고만 붙고 판정은 다른 규칙대로.
    // (윈도우 러너는 새 버퍼 1GB 의 첫 쓰기(페이지 채우기)만 2초 넘게 걸려, 한 단계도 못 끝내면 "검사 0" 규칙으로 FAIL 이다)
    let out = output(&["mem", "--seconds", "3", "--mb", "1024", "--threads", "1"]);
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(j["warnings"], serde_json::json!(["mem_base_incomplete"]), "{j}");
    assert_eq!(j["mem"]["base_complete"], false);
    // 첫 원소 뒤로 끝낸 원소가 없으면 예상은 null
    assert!(j["mem"]["base_seconds_estimate"].as_f64().is_none_or(|e| e > 3.0), "{j}");
    let checked = j["mem"]["min_thread_passes"].as_u64().unwrap() >= 1;
    assert_eq!((out.status.code(), j["verdict"].as_str()), if checked { (Some(0), Some("PASS")) } else { (Some(1), Some("FAIL")) }, "{j}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("경고: 메모리 기본 검사를 시간 안에 끝내지 못했습니다"), "{err}");
}

#[test]
fn require_complete_exit_codes() {
    // 1MB 를 2초: 기본 세트·D 4회차를 넉넉히 끝낸다 → PASS(0)
    let (code, j) = run(&["mem", "--seconds", "2", "--mb", "1", "--threads", "1", "--require-complete"]);
    assert_eq!((code, j["verdict"].as_str()), (0, Some("PASS")), "{j}");
    // 1GB 를 1초: 기본 세트를 못 끝낸다 → INCOMPLETE(4).
    // (윈도우 러너는 새 버퍼 1GB 의 첫 쓰기가 느려 한 단계도 못 끝낼 수 있다 — 그때는 "검사 0" 규칙으로 FAIL(1))
    let (code, j) = run(&["mem", "--seconds", "1", "--mb", "1024", "--threads", "2", "--require-complete"]);
    let checked = j["mem"]["min_thread_passes"].as_u64().unwrap() >= 1;
    assert_eq!((code, j["verdict"].as_str()), if checked { (4, Some("INCOMPLETE")) } else { (1, Some("FAIL")) }, "{j}");
    // 옵션이 없으면 같은 실행이 예전처럼 PASS(0) + 경고 (한 단계도 못 끝냈으면 FAIL(1))
    let (code, j) = run(&["mem", "--seconds", "1", "--mb", "1024", "--threads", "2"]);
    let checked = j["mem"]["min_thread_passes"].as_u64().unwrap() >= 1;
    assert_eq!((code, j["verdict"].as_str()), if checked { (0, Some("PASS")) } else { (1, Some("FAIL")) }, "{j}");
}

#[test]
fn mem_with_enough_time_has_no_warning() {
    let (code, j) = run(&["mem", "--seconds", "2", "--mb", "8", "--threads", "2"]);
    assert_eq!(code, 0, "{j}");
    assert!(j.get("warnings").is_none(), "{j}");
    assert_eq!(j["mem"]["base_complete"], true);
    assert!(j["mem"]["rounds_d"].as_u64().unwrap() >= 1 && j["mem"]["bursts_e"].as_u64().unwrap() >= 1, "{j}");
}

#[test]
fn mem_auto_size() {
    let out = output(&["mem", "--seconds", "1", "--mb", "auto"]);
    let err = String::from_utf8_lossy(&out.stderr);
    if cfg!(windows) {
        // 사용 가능한 메모리에서 여유를 뺀 크기를 실제로 잡아 돌린다. 큰 버퍼의 첫 쓰기가 1초를 넘으면 한 단계도 못 끝내 FAIL 일 수 있다
        let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let bytes = j["mem"]["bytes"].as_u64().unwrap();
        assert!(bytes >= 64 << 20 && bytes.is_multiple_of(1 << 20), "{j}");
        let checked = j["mem"]["min_thread_passes"].as_u64().unwrap() >= 1;
        assert_eq!(out.status.code(), Some(if checked { 0 } else { 1 }), "{j}");
        eprintln!("--mb auto: {} MiB", bytes >> 20);
    } else {
        assert_eq!(out.status.code(), Some(3));
        assert!(err.contains("--mb auto 는 윈도우 전용입니다"), "{err}");
    }
}

#[test]
fn keep_going_reports_error_list() {
    // 주입은 한 번뿐이라 계속 돌아도 오류는 1개, 시간 끝까지 돈다
    let (code, j) = run(&["mem", "--seconds", "3", "--mb", "8", "--inject-mem", "1:500", "--keep-going", "10"]);
    assert_eq!(code, 1, "{j}");
    assert_eq!(j["mem"]["errors_total"], 1);
    assert_eq!(j["mem"]["errors"].as_array().unwrap().len(), 1);
    assert_eq!(j["mem"]["errors"][0]["offset_bytes"], 4000);
    assert_eq!(j["mem"]["error"]["offset_bytes"], 4000);
    // 기본 실행에는 목록이 없다
    let (_, j) = run(&["mem", "--seconds", "2", "--mb", "8", "--inject-mem", "1:500"]);
    assert!(j["mem"].get("errors").is_none(), "{j}");
    assert_eq!(j["mem"]["errors_total"], 1);
}

#[cfg(windows)]
#[test]
fn whea_field_on_windows() {
    let (code, j) = run(&["mem", "--seconds", "1", "--mb", "8"]);
    assert_eq!(code, 0, "{j}");
    assert!(j["whea"]["during_run"].is_object() && j["whea"]["before_7_days"].is_u64(), "{j}");
}
