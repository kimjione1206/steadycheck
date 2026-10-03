//! 판정·보고서·종료 코드.

use crate::cli::Mode;
use crate::cpu::CpuOutcome;
use crate::mem::MemOutcome;
use crate::share::ShareOutcome;

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
    /// 판정은 바꾸지 않는 경고 (없으면 JSON 에서 생략). "mem_base_incomplete" = 메모리 기본 세트를 시간 안에 못 끝냄
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<CpuOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share: Option<ShareOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem: Option<MemOutcome>,
}

impl Report {
    pub fn new(mode: Mode, injected: bool, logical_cpus: usize, cpu: Option<CpuOutcome>, share: Option<ShareOutcome>, mem: Option<MemOutcome>) -> Report {
        // 아무것도 검사하지 않고 PASS 하면 안 된다
        // 코어 하나라도 한 블록도 못 돌았으면 모든 코어를 검사했다고 할 수 없다
        // 주고받기도 일꾼 하나라도 한 통도 못 받았으면 실패
        // 메모리도 일꾼 하나라도 한 패스를 못 끝냈거나 검사한 바이트가 0 이면 전체를 검사했다고 할 수 없다
        let empty = cpu.as_ref().is_some_and(|c| c.min_thread_blocks == 0)
            || share.as_ref().is_some_and(|s| s.min_thread_messages == 0)
            || mem.as_ref().is_some_and(|m| m.min_thread_passes == 0 || m.bytes_verified == 0);
        let failed = empty || cpu.as_ref().is_some_and(|c| c.failed()) || share.as_ref().is_some_and(|s| s.failed()) || mem.as_ref().is_some_and(|m| m.failed());
        Report {
            tool: "steadycheck",
            version: env!("CARGO_PKG_VERSION"),
            mode,
            cpu_brand: cpu_brand(),
            logical_cpus,
            injected,
            verdict: if failed { "FAIL" } else { "PASS" },
            // 기본 세트를 못 끝내면 결합 고장 보장이 성립하지 않는다 — 판정은 그대로 두고 알린다
            warnings: if mem.as_ref().is_some_and(|m| !m.base_complete) { vec!["mem_base_incomplete"] } else { vec![] },
            cpu,
            share,
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
            bytes_verified: min_thread_passes << 20, verified_bytes_per_sec: 0, elapsed_ms: 1000,
            base_complete: true, base_seconds_estimate: Some(0.5), rounds_d: 0, bursts_e: 0, error: None,
        }
    }

    fn share(min_thread_messages: u64) -> ShareOutcome {
        ShareOutcome {
            threads: 2, pinned: true, messages: min_thread_messages * 2, min_thread_messages,
            counter_ok: true, messages_per_sec: 0, elapsed_ms: 1000, error: None,
        }
    }

    #[test]
    fn clean_run_passes() {
        assert_eq!(Report::new(Mode::All, false, 2, Some(cpu(2, 2)), Some(share(1)), Some(mem(1))).verdict, "PASS");
    }

    #[test]
    fn cpu_worker_that_checked_nothing_fails() {
        assert_eq!(Report::new(Mode::Cpu, false, 2, Some(cpu(2, 1)), None, None).verdict, "FAIL");
        assert_eq!(Report::new(Mode::Cpu, false, 2, Some(cpu(2, 0)), None, None).verdict, "FAIL");
    }

    #[test]
    fn one_idle_worker_fails_even_if_total_is_enough() {
        // 스레드 4, 합계 블록 10 이지만 한 워커는 0 블록
        let mut c = cpu(4, 10);
        c.min_thread_blocks = 0;
        let rep = Report::new(Mode::Cpu, false, 4, Some(c), None, None);
        assert_eq!(rep.verdict, "FAIL");
    }

    #[test]
    fn mem_that_verified_nothing_fails() {
        assert_eq!(Report::new(Mode::Mem, false, 2, None, None, Some(mem(0))).verdict, "FAIL");
    }

    #[test]
    fn one_idle_mem_worker_fails() {
        let mut m = mem(3);
        m.min_thread_passes = 0;
        assert_eq!(Report::new(Mode::Mem, false, 2, None, None, Some(m)).verdict, "FAIL");
    }

    #[test]
    fn mem_with_passes_but_no_bytes_fails() {
        // 버퍼가 0 바이트면 빈 패스만 돌고 아무것도 검사하지 않는다
        let mut m = mem(3);
        m.bytes_verified = 0;
        assert_eq!(Report::new(Mode::Mem, false, 2, None, None, Some(m)).verdict, "FAIL");
    }

    #[test]
    fn base_incomplete_warns_but_keeps_verdict() {
        let rep = Report::new(Mode::Mem, false, 2, None, None, Some(mem(3)));
        assert!(rep.warnings.is_empty());
        let mut m = mem(3);
        m.base_complete = false;
        let rep = Report::new(Mode::Mem, false, 2, None, None, Some(m));
        assert_eq!((rep.verdict, rep.warnings), ("PASS", vec!["mem_base_incomplete"]));
        // 오류가 있으면 FAIL 이고 경고도 함께 남는다
        let mut m = mem(3);
        m.base_complete = false;
        m.min_thread_passes = 0;
        assert_eq!(Report::new(Mode::Mem, false, 2, None, None, Some(m)).verdict, "FAIL");
        // 메모리를 안 돌았으면 경고 없음, JSON 에서도 빠진다
        let rep = Report::new(Mode::Cpu, false, 2, Some(cpu(2, 2)), None, None);
        assert!(!serde_json::to_string(&rep).unwrap().contains("warnings"));
    }

    #[test]
    fn share_verdicts() {
        assert_eq!(Report::new(Mode::Share, false, 2, None, Some(share(5)), None).verdict, "PASS");
        // 주입한 옛 값이 잡히면 실패
        let mut s = share(5);
        s.error = Some(crate::share::ShareError {
            cpu: 1, from: 0, seq: 5, word: 0, expected: "0x1".into(), actual: "0x2".into(), at_ms: 1,
        });
        assert_eq!(Report::new(Mode::Share, true, 2, None, Some(s), None).verdict, "FAIL");
        // 한 통도 못 받은 일꾼이 있으면 실패
        assert_eq!(Report::new(Mode::Share, false, 2, None, Some(share(0)), None).verdict, "FAIL");
        // 공용 카운터 합이 어긋나면 실패
        let mut s = share(5);
        s.counter_ok = false;
        assert_eq!(Report::new(Mode::Share, false, 2, None, Some(s), None).verdict, "FAIL");
    }

    #[test]
    fn cpu_brand_is_not_empty() {
        assert!(!cpu_brand().is_empty());
        #[cfg(not(target_arch = "x86_64"))]
        assert_eq!(cpu_brand(), "unknown (non-x86)");
    }

    // x86 에서는 실제 이름이 나오고 앞뒤 '\0'·공백이 깎여 있어야 함
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn cpu_brand_is_trimmed_name_on_x86() {
        let b = cpu_brand();
        assert_ne!(b, "unknown");
        assert_eq!(b, b.trim_matches(|c: char| c == '\0' || c.is_whitespace()));
    }
}
