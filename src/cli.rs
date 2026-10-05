//! 인자 파싱. 외부 크레이트 없이 "모드 + --옵션 값" 쌍만 받는다.

use crate::cpu::{CpuInject, KernelSet, Pattern};
use crate::kernel::{Flip, Isa};
use crate::mem::MemInject;
use crate::share::ShareInject;

pub const USAGE: &str = "사용법: steadycheck <cpu|share|mem|all> [--seconds N] [--threads N] [--isa auto|scalar|avx2|avx512] [--kernel mix|chain|wide|fma|fma32|lz] [--pattern steady|pulse|cycle] [--mb N|auto] [--iters N] [--inject-cpu CPU:BLOCK] [--inject-mem PASS:WORD] [--inject-share CPU:MSG] [--require-complete] [--keep-going N]";

/// 30일
const MAX_SECONDS: u64 = 2_592_000;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Cpu,
    Share,
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
    /// --mb auto: 크기는 실행할 때 정한다(auto_mb, 윈도우 전용)
    pub mb_auto: bool,
    pub iters: Option<u64>,
    pub kernels: KernelSet,
    pub pattern: Pattern,
    pub inject_cpu: Option<CpuInject>,
    pub inject_mem: Option<MemInject>,
    pub inject_share: Option<ShareInject>,
    pub require_complete: bool,
    /// --keep-going N: 메모리 오류를 N개까지 모은 뒤 멈춘다 (없으면 첫 오류에서)
    pub keep_going: Option<u64>,
}

pub fn parse(argv: &[String]) -> Result<Args, String> {
    let mut it = argv.iter();
    let mode = match it.next().map(String::as_str) {
        Some("cpu") => Mode::Cpu,
        Some("share") => Mode::Share,
        Some("mem") => Mode::Mem,
        Some("all") => Mode::All,
        other => return Err(format!("알 수 없는 모드: {other:?}")),
    };
    let mut a = Args { mode, seconds: 60, threads: None, isa: None, mb: 1024, mb_auto: false, iters: None, kernels: KernelSet::Mix, pattern: Pattern::Steady, inject_cpu: None, inject_mem: None, inject_share: None, require_complete: false, keep_going: None };
    while let Some(flag) = it.next() {
        // 값 없는 깃발
        if flag == "--require-complete" {
            a.require_complete = true;
            continue;
        }
        let val = it.next().ok_or_else(|| format!("{flag} 뒤에 값이 필요합니다"))?;
        match flag.as_str() {
            "--seconds" => a.seconds = num(val)?,
            "--threads" => a.threads = Some(num(val)?),
            "--isa" if val == "auto" => a.isa = None,
            "--isa" => a.isa = Some(Isa::parse(val).ok_or(format!("알 수 없는 isa: {val}"))?),
            "--mb" if val == "auto" && cfg!(windows) => a.mb_auto = true,
            "--mb" if val == "auto" => return Err("--mb auto 는 윈도우 전용입니다".into()),
            // 다른 옵션처럼 뒤에 온 것이 이긴다: --mb auto 뒤의 --mb N 은 auto 를 끈다
            "--mb" => {
                a.mb = num(val)?;
                a.mb_auto = false;
            }
            "--iters" => a.iters = Some(num(val)?),
            "--kernel" => a.kernels = KernelSet::parse(val).ok_or(format!("알 수 없는 커널: {val}"))?,
            "--pattern" => a.pattern = Pattern::parse(val).ok_or(format!("알 수 없는 패턴: {val}"))?,
            "--inject-cpu" => {
                let (cpu, block) = pair(val)?;
                a.inject_cpu = Some(CpuInject { cpu: cpu as usize, block, flip: Flip { lane: 0, bit: 0 } });
            }
            "--inject-mem" => {
                let (pass, word) = pair(val)?;
                a.inject_mem = Some(MemInject { pass, word: word as usize, bit: 0, late: false });
            }
            "--inject-share" => {
                let (cpu, msg) = pair(val)?;
                a.inject_share = Some(ShareInject { cpu: cpu as usize, msg });
            }
            "--keep-going" => a.keep_going = Some(num(val)?),
            _ => return Err(format!("알 수 없는 옵션: {flag}")),
        }
    }
    if a.seconds == 0 || a.mb == 0 || a.iters == Some(0) || a.threads == Some(0) || a.keep_going == Some(0) {
        return Err("0 은 쓸 수 없습니다".into());
    }
    if a.keep_going.is_some_and(|n| n > 1000) {
        return Err("--keep-going 은 1000 이하여야 합니다".into());
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

/// --mb auto 의 크기: 사용 가능한 실제 메모리(바이트)에서 여유 max(1GiB, 10%) 를 뺀 MiB, 최소 64
pub fn auto_mb_from(avail_bytes: u64) -> usize {
    let reserve = (1u64 << 30).max(avail_bytes / 10);
    ((avail_bytes.saturating_sub(reserve) >> 20) as usize).max(64)
}

/// 윈도우의 (사용 가능한 실제 메모리, 전체 실제 메모리) 바이트
#[cfg(windows)]
fn phys_mem() -> Result<(u64, u64), String> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut st: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut st) } == 0 {
        return Err("사용 가능한 메모리를 읽지 못했습니다".into());
    }
    Ok((st.ullAvailPhys, st.ullTotalPhys))
}

/// --mb auto: 윈도우의 사용 가능한 실제 메모리(GlobalMemoryStatusEx 의 ullAvailPhys)로 정한다. 읽지 못하면 환경 오류
#[cfg(windows)]
pub fn auto_mb() -> Result<usize, String> {
    Ok(auto_mb_from(phys_mem()?.0))
}

/// 전체 실제 메모리 바이트 (윈도우 외·읽기 실패면 None)
pub fn total_phys_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        phys_mem().ok().map(|(_, total)| total)
    }
    #[cfg(not(windows))]
    {
        None
    }
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
    fn last_mb_wins() {
        if cfg!(windows) {
            let a = p("mem --mb auto --mb 64").unwrap();
            assert_eq!((a.mb_auto, a.mb), (false, 64));
            assert!(p("mem --mb 64 --mb auto").unwrap().mb_auto);
        } else {
            // auto 는 윈도우 전용이라 어느 순서든 사용법 오류
            assert!(p("mem --mb auto --mb 64").is_err() && p("mem --mb 64 --mb auto").is_err());
        }
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
        let a = p("share --inject-share 3:9").unwrap();
        assert_eq!(a.mode, Mode::Share);
        let s = a.inject_share.unwrap();
        assert_eq!((s.cpu, s.msg), (3, 9));
        assert_eq!(p("cpu --pattern cycle").unwrap().pattern, crate::cpu::Pattern::Cycle);
    }

    #[test]
    fn require_complete_is_a_bare_flag() {
        assert!(!p("mem").unwrap().require_complete);
        let a = p("mem --require-complete --seconds 5").unwrap();
        assert!(a.require_complete && a.seconds == 5);
        assert!(p("mem --seconds 5 --require-complete").unwrap().require_complete);
    }

    #[test]
    fn keep_going_bounds() {
        assert_eq!(p("mem").unwrap().keep_going, None);
        assert_eq!(p("mem --keep-going 20").unwrap().keep_going, Some(20));
        assert_eq!(p("mem --keep-going 1000").unwrap().keep_going, Some(1000));
        assert!(p("mem --keep-going 0").is_err());
        assert!(p("mem --keep-going 1001").is_err());
        assert!(p("mem --keep-going x").is_err());
    }

    #[test]
    fn auto_mb_keeps_a_reserve() {
        const G: u64 = 1 << 30;
        // 여유는 1GiB 와 10% 중 큰 쪽
        // 32GiB: 여유 10% = 3,435,973,836 바이트 → 남는 30,923,764,532 바이트 = 29,491.2 MiB → 29,491
        assert_eq!(auto_mb_from(32 * G), 29_491);
        assert_eq!(auto_mb_from(8 * G), 7 * 1024);
        assert_eq!(auto_mb_from(10 * G), 9 * 1024);
        // 최소 64MiB
        assert_eq!((auto_mb_from(G), auto_mb_from(0), auto_mb_from(G + (10 << 20))), (64, 64, 64));
    }

    #[test]
    fn mb_auto_is_windows_only() {
        let r = p("mem --mb auto");
        #[cfg(windows)]
        {
            assert!(r.unwrap().mb_auto);
            assert!(auto_mb().unwrap() >= 64);
        }
        #[cfg(not(windows))]
        assert_eq!(r.unwrap_err(), "--mb auto 는 윈도우 전용입니다");
        assert!(!p("mem --mb 64").unwrap().mb_auto);
    }

    #[test]
    fn rejects_bad_input() {
        for bad in ["", "gpu", "cpu --seconds", "cpu --seconds x", "cpu --seconds 0", "cpu --isa sse",
                    "cpu --inject-cpu 3", "share --inject-share 1", "cpu --bogus 1", "mem --mb 0", "cpu --threads 0",
                    "mem --mb 17592186044416", "cpu --seconds 2592001", "cpu --seconds 18446744073709551615",
                    "cpu --kernel avx", "cpu --pattern burst", "cpu --iters 0"] {
            assert!(p(bad).is_err(), "받아들이면 안 됨: {bad:?}");
        }
    }
}
