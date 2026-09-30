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
