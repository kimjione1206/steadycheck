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
        let failed = cpu.as_ref().is_some_and(|c| c.failed()) || mem.as_ref().is_some_and(|m| m.failed());
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
