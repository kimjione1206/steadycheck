use std::time::Duration;
use steadycheck::{cli, cpu, kernel::Isa, mem, report};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match cli::parse(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n{}", cli::USAGE);
            std::process::exit(report::EXIT_USAGE);
        }
    };
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
    // 첫 오류에서 멈춘다: CPU 가 이미 틀렸으면 RAM 검사는 건너뛴다
    let cpu_failed = cpu_out.as_ref().is_some_and(|c| c.failed());
    let mem_out = (matches!(args.mode, cli::Mode::Mem | cli::Mode::All) && !cpu_failed)
        .then(|| mem::run(&mem::MemConfig { mb: args.mb, duration, inject: args.inject_mem }));

    let injected = args.inject_cpu.is_some() || args.inject_mem.is_some();
    let rep = report::Report::new(args.mode, injected, logical, cpu_out, mem_out);
    println!("{}", serde_json::to_string_pretty(&rep).expect("JSON 변환"));
    eprintln!("판정: {}", rep.verdict);
    std::process::exit(if rep.verdict == "PASS" { report::EXIT_PASS } else { report::EXIT_FAIL });
}
