//! 판정·보고서·종료 코드.

use crate::cli::Mode;
use crate::cpu::CpuOutcome;
use crate::mem::MemOutcome;

pub const EXIT_PASS: i32 = 0;
pub const EXIT_FAIL: i32 = 1;
pub const EXIT_ENV: i32 = 2;
pub const EXIT_USAGE: i32 = 3;

#[derive(serde::Serialize)]
pub struct Report {
    pub tool: &'static str,
    pub version: &'static str,
    pub mode: Mode,
    pub cpu_brand: String,
    pub logical_cpus: usize,
    pub injected: bool,
    pub verdict: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<CpuOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem: Option<MemOutcome>,
}

impl Report {
    pub fn new(mode: Mode, injected: bool, logical_cpus: usize, cpu: Option<CpuOutcome>, mem: Option<MemOutcome>) -> Report {
        // 아무것도 검사하지 않고 PASS 하면 안 된다
        // 코어 하나라도 한 블록도 못 돌았으면 모든 코어를 검사했다고 할 수 없다
        // 메모리도 일꾼 하나라도 한 패스를 못 끝냈으면 전체를 검사했다고 할 수 없다
        let empty = cpu.as_ref().is_some_and(|c| c.min_thread_blocks == 0)
            || mem.as_ref().is_some_and(|m| m.min_thread_passes == 0);
        let failed = empty || cpu.as_ref().is_some_and(|c| c.failed()) || mem.as_ref().is_some_and(|m| m.failed());
        Report {
            tool: "steadycheck",
            version: env!("CARGO_PKG_VERSION"),
            mode,
            cpu_brand: cpu_brand(),
            logical_cpus,
            injected,
            verdict: if failed { "FAIL" } else { "PASS" },
            cpu,
            mem,
        }
    }
}

/// CPU 이름(예: "AMD EPYC 7763 64-Core Processor"). CI 에서 어떤 기종이 걸렸는지 기록용.
#[allow(unused_unsafe)]
pub fn cpu_brand() -> String {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::__cpuid;
        if unsafe { __cpuid(0x8000_0000) }.eax < 0x8000_0004 {
            return "unknown".into();
        }
        let mut bytes = Vec::new();
        for leaf in 0x8000_0002u32..=0x8000_0004 {
            let r = unsafe { __cpuid(leaf) };
            for v in [r.eax, r.ebx, r.ecx, r.edx] {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        String::from_utf8_lossy(&bytes).trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        "unknown (non-x86)".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{KernelSet, Pattern};
    use crate::kernel::Isa;

    fn cpu(threads: usize, blocks: u64) -> CpuOutcome {
        CpuOutcome {
            isa: Isa::Scalar, rotate_isa: false, kernels: KernelSet::Chain, pattern: Pattern::Steady, threads, pinned: true, blocks,
            min_thread_blocks: blocks / threads.max(1) as u64,
            lane_iters: 0, run_ms: 1000, lane_iters_per_sec: 0, elapsed_ms: 1000, golden_unstable: false, error: None,
        }
    }

    fn mem(min_thread_passes: u64) -> MemOutcome {
        MemOutcome {
            bytes: 1 << 20, threads: 2, pinned: true, passes: min_thread_passes * 2, min_thread_passes,
            bytes_verified: min_thread_passes << 20, verified_bytes_per_sec: 0, elapsed_ms: 1000, error: None,
        }
    }

    #[test]
    fn clean_run_passes() {
        assert_eq!(Report::new(Mode::All, false, 2, Some(cpu(2, 2)), Some(mem(1))).verdict, "PASS");
    }

    #[test]
    fn cpu_worker_that_checked_nothing_fails() {
        assert_eq!(Report::new(Mode::Cpu, false, 2, Some(cpu(2, 1)), None).verdict, "FAIL");
        assert_eq!(Report::new(Mode::Cpu, false, 2, Some(cpu(2, 0)), None).verdict, "FAIL");
    }

    #[test]
    fn one_idle_worker_fails_even_if_total_is_enough() {
        // 스레드 4, 합계 블록 10 이지만 한 워커는 0 블록
        let mut c = cpu(4, 10);
        c.min_thread_blocks = 0;
        let rep = Report::new(Mode::Cpu, false, 4, Some(c), None);
        assert_eq!(rep.verdict, "FAIL");
    }

    #[test]
    fn mem_that_verified_nothing_fails() {
        assert_eq!(Report::new(Mode::Mem, false, 2, None, Some(mem(0))).verdict, "FAIL");
    }

    #[test]
    fn one_idle_mem_worker_fails() {
        let mut m = mem(3);
        m.min_thread_passes = 0;
        assert_eq!(Report::new(Mode::Mem, false, 2, None, Some(m)).verdict, "FAIL");
    }

    #[test]
    fn cpu_brand_is_not_empty() {
        assert!(!cpu_brand().is_empty());
    }
}
