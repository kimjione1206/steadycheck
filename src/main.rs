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
    let isa = args.isa.unwrap_or_else(Isa::best);
    if !isa.supported() {
        eprintln!("이 CPU 는 {isa:?} 를 지원하지 않습니다");
        std::process::exit(report::EXIT_ENV);
    }
    let logical = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let duration = Duration::from_secs(args.seconds);

    let cpu_out = matches!(args.mode, cli::Mode::Cpu | cli::Mode::All).then(|| {
        cpu::run(&cpu::CpuConfig {
            isa, threads: args.threads.unwrap_or(logical), duration,
            kernels: args.kernels, pattern: args.pattern,
            iters: args.iters, inject: args.inject_cpu,
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
