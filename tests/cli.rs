use std::process::Command;

fn run(args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_steadycheck")).args(args).output().unwrap();
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
    for k in ["chain", "wide", "fma", "fma32", "mix"] {
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
    let out = Command::new(env!("CARGO_BIN_EXE_steadycheck"))
        .args(["cpu", "--pattern", "cycle", "--threads", "16", "--seconds", "1"])
        .output()
        .unwrap();
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
