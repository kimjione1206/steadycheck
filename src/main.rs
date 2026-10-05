use std::time::Duration;
use steadycheck::{cli, cpu, kernel::Isa, mem, report, share, whea};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match cli::parse(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n{}", cli::USAGE);
            std::process::exit(report::EXIT_USAGE);
        }
    };
    let started = std::time::Instant::now();
    let logical = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let duration = Duration::from_secs(args.seconds);
    let threads = args.threads.unwrap_or(logical);
    let cpu_mode = matches!(args.mode, cli::Mode::Cpu | cli::Mode::All);
    // 코어 순환이 너무 짧으면 차례를 못 받은 코어 때문에 불량처럼 보이므로 사용법 오류로 막는다
    if cpu_mode && args.pattern == cpu::Pattern::Cycle {
        let min_ms = (threads as u64).saturating_mul(cpu::CYCLE_MIN_MS);
        if args.seconds.saturating_mul(1000) < min_ms {
            eprintln!("코어 순환은 모든 코어({threads}개)가 차례를 받도록 최소 {}초가 필요합니다\n{}", min_ms.div_ceil(1000), cli::USAGE);
            std::process::exit(report::EXIT_USAGE);
        }
    }
    // --mb auto: 사용 가능한 메모리를 읽지 못하면 환경 오류
    #[cfg(windows)]
    let mb = if args.mb_auto {
        match cli::auto_mb() {
            Ok(mb) => mb,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(report::EXIT_ENV);
            }
        }
    } else {
        args.mb
    };
    #[cfg(not(windows))]
    let mb = args.mb;
    let isa = args.isa.unwrap_or_else(Isa::best);
    if !isa.supported() {
        eprintln!("이 CPU 는 {isa:?} 를 지원하지 않습니다");
        std::process::exit(report::EXIT_ENV);
    }

    let cpu_out = cpu_mode.then(|| {
        cpu::run(&cpu::CpuConfig {
            isa, threads, duration,
            kernels: args.kernels, pattern: args.pattern,
            iters: args.iters, inject: args.inject_cpu, rotate_isa: args.isa.is_none(), fault: None,
        })
    });
    // 첫 오류에서 멈춘다: CPU 가 이미 틀렸으면 뒤 검사는 건너뛴다
    let cpu_failed = cpu_out.as_ref().is_some_and(|c| c.failed());
    // 주고받기 일꾼도 CPU 보다 많으면 상대가 차례를 못 받아 거짓 FAIL 이 난다
    let share_out = (matches!(args.mode, cli::Mode::Share | cli::Mode::All) && !cpu_failed)
        .then(|| share::run(&share::ShareConfig { threads: threads.min(logical), duration, inject: args.inject_share }));
    let share_failed = share_out.as_ref().is_some_and(|s| s.failed());
    // CPU 보다 많은 메모리 일꾼은 전송량을 늘리지 못하고 차례를 못 받아 거짓 FAIL 만 만든다
    let mut mem_out = (matches!(args.mode, cli::Mode::Mem | cli::Mode::All) && !cpu_failed && !share_failed)
        .then(|| mem::run(&mem::MemConfig { mb, duration, threads: threads.min(logical), inject: args.inject_mem, fault: None, max_errors: args.keep_going.unwrap_or(1) }));
    if let (Some(m), Some(total)) = (mem_out.as_mut(), cli::total_phys_bytes()) {
        m.total_phys_bytes = Some(total);
        m.tested_percent = Some(mem::tested_percent(m.bytes as u64, total));
    }

    let injected = args.inject_cpu.is_some() || args.inject_mem.is_some() || args.inject_share.is_some();
    let rep = report::Report::new(args.mode, injected, logical, cpu_out, share_out, mem_out);
    let mut rep = if args.require_complete { rep.require_complete() } else { rep };
    rep.whea = whea::query(started.elapsed().as_millis() as u64);
    println!("{}", serde_json::to_string_pretty(&rep).expect("JSON 변환"));
    if rep.warnings.contains(&"mem_base_incomplete") {
        let est = rep.mem.as_ref().and_then(|m| m.base_seconds_estimate).map_or("알 수 없음".to_string(), |s| format!("약 {s}초"));
        eprintln!("경고: 메모리 기본 검사를 시간 안에 끝내지 못했습니다({est} 걸림 예상) — 결합 고장 보장이 성립하지 않으니 --seconds 를 늘리세요");
    }
    if rep.verdict == "INCOMPLETE" {
        eprintln!("메모리 검사가 덜 됐습니다(기본 세트 미완료 또는 D 회차 {} 미만) — --seconds 를 늘려 다시 돌리세요", report::MIN_ROUNDS_D);
    }
    if let Some(n) = rep.whea.as_ref().map(|w| w.during_run.values().sum::<u64>()).filter(|&n| n > 0) {
        eprintln!("참고: 검사하는 동안 윈도우 하드웨어 오류 기록(WHEA)이 {n}건 남았습니다 — 판정과 별개로 이벤트 뷰어에서 확인하세요");
    }
    eprintln!("판정: {}", rep.verdict);
    std::process::exit(match rep.verdict {
        "PASS" => report::EXIT_PASS,
        "INCOMPLETE" => report::EXIT_INCOMPLETE,
        _ => report::EXIT_FAIL,
    });
}
