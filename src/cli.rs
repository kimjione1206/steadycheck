//! 인자 파싱. 외부 크레이트 없이 "모드 + --옵션 값" 쌍만 받는다.

use crate::cpu::{CpuInject, KernelSet, Pattern};
use crate::kernel::{Flip, Isa};
use crate::mem::MemInject;

pub const USAGE: &str = "사용법: steadycheck <cpu|mem|all> [--seconds N] [--threads N] [--isa auto|scalar|avx2|avx512] [--kernel mix|chain|wide|fma|fma32] [--pattern steady|pulse] [--mb N] [--iters N] [--inject-cpu CPU:BLOCK] [--inject-mem PASS:WORD]";

/// 30일
const MAX_SECONDS: u64 = 2_592_000;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Cpu,
    Mem,
    All,
}

#[derive(Debug)]
pub struct Args {
    pub mode: Mode,
    pub seconds: u64,
    pub threads: Option<usize>,
    pub isa: Option<Isa>,
    pub mb: usize,
    pub iters: Option<u64>,
    pub kernels: KernelSet,
    pub pattern: Pattern,
    pub inject_cpu: Option<CpuInject>,
    pub inject_mem: Option<MemInject>,
}

pub fn parse(argv: &[String]) -> Result<Args, String> {
    let mut it = argv.iter();
    let mode = match it.next().map(String::as_str) {
        Some("cpu") => Mode::Cpu,
        Some("mem") => Mode::Mem,
        Some("all") => Mode::All,
        other => return Err(format!("알 수 없는 모드: {other:?}")),
    };
    let mut a = Args { mode, seconds: 60, threads: None, isa: None, mb: 1024, iters: None, kernels: KernelSet::Mix, pattern: Pattern::Steady, inject_cpu: None, inject_mem: None };
    while let Some(flag) = it.next() {
        let val = it.next().ok_or_else(|| format!("{flag} 뒤에 값이 필요합니다"))?;
        match flag.as_str() {
            "--seconds" => a.seconds = num(val)?,
            "--threads" => a.threads = Some(num(val)?),
            "--isa" if val == "auto" => a.isa = None,
            "--isa" => a.isa = Some(Isa::parse(val).ok_or(format!("알 수 없는 isa: {val}"))?),
            "--mb" => a.mb = num(val)?,
            "--iters" => a.iters = Some(num(val)?),
            "--kernel" => a.kernels = KernelSet::parse(val).ok_or(format!("알 수 없는 커널: {val}"))?,
            "--pattern" => a.pattern = Pattern::parse(val).ok_or(format!("알 수 없는 패턴: {val}"))?,
            "--inject-cpu" => {
                let (cpu, block) = pair(val)?;
                a.inject_cpu = Some(CpuInject { cpu: cpu as usize, block, flip: Flip { lane: 0, bit: 0 } });
            }
            "--inject-mem" => {
                let (pass, word) = pair(val)?;
                a.inject_mem = Some(MemInject { pass, word: word as usize, bit: 0 });
            }
            _ => return Err(format!("알 수 없는 옵션: {flag}")),
        }
    }
    if a.seconds == 0 || a.mb == 0 || a.iters == Some(0) || a.threads == Some(0) {
        return Err("0 은 쓸 수 없습니다".into());
    }
    // 넘침 방지: 바이트 수가 usize 를 넘거나 30일을 넘으면 거부
    if a.mb.checked_mul(1024 * 1024).is_none() {
        return Err(format!("--mb 가 너무 큽니다: {}", a.mb));
    }
    if a.seconds > MAX_SECONDS {
        return Err(format!("--seconds 는 {MAX_SECONDS} 이하여야 합니다"));
    }
    Ok(a)
}

fn num<T: std::str::FromStr>(s: &str) -> Result<T, String> {
    s.parse().map_err(|_| format!("숫자가 아닙니다: {s}"))
}

fn pair(s: &str) -> Result<(u64, u64), String> {
    let (a, b) = s.split_once(':').ok_or(format!("A:B 형식이어야 합니다: {s}"))?;
    Ok((num(a)?, num(b)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Args, String> {
        parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn defaults() {
        let a = p("cpu").unwrap();
        assert_eq!(a.mode, Mode::Cpu);
        assert_eq!((a.seconds, a.mb, a.iters), (60, 1024, None));
        assert_eq!((a.kernels, a.pattern), (crate::cpu::KernelSet::Mix, crate::cpu::Pattern::Steady));
        assert!(a.isa.is_none() && a.threads.is_none() && a.inject_cpu.is_none());
    }

    #[test]
    fn options() {
        let a = p("all --seconds 5 --threads 3 --isa avx2 --mb 64 --iters 4096 --inject-cpu 1:7 --inject-mem 2:99 --kernel fma --pattern pulse").unwrap();
        assert_eq!(a.mode, Mode::All);
        assert_eq!((a.seconds, a.threads, a.mb, a.iters), (5, Some(3), 64, Some(4096)));
        assert_eq!((a.kernels, a.pattern), (crate::cpu::KernelSet::Fma, crate::cpu::Pattern::Pulse));
        assert_eq!(a.isa, Some(crate::kernel::Isa::Avx2));
        let c = a.inject_cpu.unwrap();
        assert_eq!((c.cpu, c.block), (1, 7));
        let m = a.inject_mem.unwrap();
        assert_eq!((m.pass, m.word), (2, 99));
        assert_eq!(p("mem --isa auto").unwrap().isa, None);
    }

    #[test]
    fn rejects_bad_input() {
        for bad in ["", "gpu", "cpu --seconds", "cpu --seconds x", "cpu --seconds 0", "cpu --isa sse",
                    "cpu --inject-cpu 3", "cpu --bogus 1", "mem --mb 0", "cpu --threads 0",
                    "mem --mb 17592186044416", "cpu --seconds 2592001", "cpu --seconds 18446744073709551615",
                    "cpu --kernel avx", "cpu --pattern burst", "cpu --iters 0"] {
            assert!(p(bad).is_err(), "받아들이면 안 됨: {bad:?}");
        }
    }
}
