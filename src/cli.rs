//! 인자 파싱. 외부 크레이트 없이 "모드 + --옵션 값" 쌍만 받는다.

use crate::cpu::CpuInject;
use crate::kernel::{Flip, Isa, ITERS_PER_BLOCK};
use crate::mem::MemInject;

pub const USAGE: &str = "사용법: steadycheck <cpu|mem|all> [--seconds N] [--threads N] [--isa auto|scalar|avx2|avx512] [--mb N] [--iters N] [--inject-cpu CPU:BLOCK] [--inject-mem PASS:WORD]";

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
    pub iters: u64,
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
    let mut a = Args { mode, seconds: 60, threads: None, isa: None, mb: 1024, iters: ITERS_PER_BLOCK, inject_cpu: None, inject_mem: None };
    while let Some(flag) = it.next() {
        let val = it.next().ok_or_else(|| format!("{flag} 뒤에 값이 필요합니다"))?;
        match flag.as_str() {
            "--seconds" => a.seconds = num(val)?,
            "--threads" => a.threads = Some(num(val)?),
            "--isa" if val == "auto" => a.isa = None,
            "--isa" => a.isa = Some(Isa::parse(val).ok_or(format!("알 수 없는 isa: {val}"))?),
            "--mb" => a.mb = num(val)?,
            "--iters" => a.iters = num(val)?,
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
    if a.seconds == 0 || a.mb == 0 || a.iters == 0 || a.threads == Some(0) {
        return Err("0 은 쓸 수 없습니다".into());
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
        assert_eq!((a.seconds, a.mb, a.iters), (60, 1024, crate::kernel::ITERS_PER_BLOCK));
        assert!(a.isa.is_none() && a.threads.is_none() && a.inject_cpu.is_none());
    }

    #[test]
    fn options() {
        let a = p("all --seconds 5 --threads 3 --isa avx2 --mb 64 --iters 4096 --inject-cpu 1:7 --inject-mem 2:99").unwrap();
        assert_eq!(a.mode, Mode::All);
        assert_eq!((a.seconds, a.threads, a.mb, a.iters), (5, Some(3), 64, 4096));
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
                    "cpu --inject-cpu 3", "cpu --bogus 1", "mem --mb 0", "cpu --threads 0"] {
            assert!(p(bad).is_err(), "받아들이면 안 됨: {bad:?}");
        }
    }
}
