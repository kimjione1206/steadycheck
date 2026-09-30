//! RAM 검사: 패턴을 전부 쓰고 전부 읽어 대조한다. 5가지 패턴을 돌아가며 반복.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct MemInject {
    pub pass: u64,
    pub word: usize,
    pub bit: u32,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub struct MemError {
    pub pass: u64,
    pub pattern: String,
    pub offset_bytes: usize,
    pub expected: String,
    pub actual: String,
    pub reread: String,
    pub pass_start_ms: u64,
    pub at_ms: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct MemOutcome {
    pub bytes: usize,
    pub passes: u64,
    pub bytes_verified: u64,
    pub elapsed_ms: u64,
    pub error: Option<MemError>,
}

impl MemOutcome {
    pub fn failed(&self) -> bool {
        self.error.is_some()
    }
}

pub struct MemConfig {
    pub mb: usize,
    pub duration: Duration,
    pub inject: Option<MemInject>,
}

#[derive(Clone, Copy)]
enum Pattern {
    Solid(u64),
    Address(u64),
}

// 패스 순서: 0101.., 1010.., 전부 0, 전부 1, 주소 섞기 — 반복
fn pattern_for(pass: u64) -> Pattern {
    match pass % 5 {
        0 => Pattern::Solid(0x5555_5555_5555_5555),
        1 => Pattern::Solid(0xAAAA_AAAA_AAAA_AAAA),
        2 => Pattern::Solid(0),
        3 => Pattern::Solid(u64::MAX),
        _ => Pattern::Address(0xD1B5_4A32_D192_ED03 ^ pass),
    }
}

fn value(p: Pattern, i: usize) -> u64 {
    match p {
        Pattern::Solid(v) => v,
        Pattern::Address(k) => (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ k,
    }
}

fn name(p: Pattern) -> String {
    match p {
        Pattern::Solid(v) => format!("solid {v:#018x}"),
        Pattern::Address(_) => "address".into(),
    }
}

pub fn run(cfg: &MemConfig) -> MemOutcome {
    let start = Instant::now();
    let ms = || start.elapsed().as_millis() as u64;
    let words = cfg.mb * 1024 * 1024 / 8;
    let mut buf = vec![0u64; words];
    let ptr = buf.as_mut_ptr();
    let mut pass = 0u64;
    let mut verified = 0u64;

    while start.elapsed() < cfg.duration {
        let p = pattern_for(pass);
        let pass_start_ms = ms();
        // volatile: 컴파일러가 "쓴 값을 그대로 안다"며 읽기를 생략하지 못하게
        for i in 0..words {
            unsafe { ptr.add(i).write_volatile(value(p, i)) }
        }
        if let Some(inj) = cfg.inject.filter(|j| j.pass == pass && j.word < words) {
            unsafe {
                let q = ptr.add(inj.word);
                q.write_volatile(q.read_volatile() ^ (1u64 << (inj.bit % 64)));
            }
        }
        for i in 0..words {
            let want = value(p, i);
            let got = unsafe { ptr.add(i).read_volatile() };
            if got != want {
                let reread = unsafe { ptr.add(i).read_volatile() };
                return MemOutcome {
                    bytes: words * 8,
                    passes: pass + 1,
                    bytes_verified: verified + i as u64 * 8,
                    elapsed_ms: ms(),
                    error: Some(MemError {
                        pass,
                        pattern: name(p),
                        offset_bytes: i * 8,
                        expected: format!("{want:#018x}"),
                        actual: format!("{got:#018x}"),
                        reread: format!("{reread:#018x}"),
                        pass_start_ms,
                        at_ms: ms(),
                    }),
                };
            }
        }
        verified += words as u64 * 8;
        pass += 1;
    }
    drop(buf);
    MemOutcome { bytes: words * 8, passes: pass, bytes_verified: verified, elapsed_ms: ms(), error: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn clean_run_has_no_error() {
        let out = run(&MemConfig { mb: 8, duration: Duration::from_millis(500), inject: None });
        assert!(out.error.is_none(), "{:?}", out.error);
        assert!(out.passes >= 1);
        assert_eq!(out.bytes_verified, out.passes * out.bytes as u64);
    }

    #[test]
    fn injected_flip_is_caught_at_exact_place() {
        let inj = MemInject { pass: 2, word: 12_345, bit: 17 };
        let out = run(&MemConfig { mb: 8, duration: Duration::from_secs(5), inject: Some(inj) });
        let e = out.error.clone().expect("주입한 오류를 잡아야 한다");
        assert_eq!((e.pass, e.offset_bytes), (2, 12_345 * 8));
        assert_ne!(e.expected, e.actual);
        assert_eq!(e.actual, e.reread, "뒤집힌 값은 다시 읽어도 같아야 한다");
        assert!(out.failed());
    }

    // 변이 테스트 보강: 오류 보고의 숫자가 정확해야 한다 (패스 3 = 전부 1)
    #[test]
    fn error_report_numbers_are_exact() {
        let bytes = 8 * 1024 * 1024;
        let inj = MemInject { pass: 3, word: 12_345, bit: 17 };
        let out = run(&MemConfig { mb: 8, duration: Duration::from_secs(5), inject: Some(inj) });
        assert_eq!((out.bytes, out.passes), (bytes, 4));
        assert_eq!(out.bytes_verified, 3 * bytes as u64 + 12_345 * 8);
        let e = out.error.expect("주입한 오류를 잡아야 한다");
        assert_eq!(e.expected, format!("{:#018x}", u64::MAX));
        assert_eq!(e.actual, format!("{:#018x}", u64::MAX ^ (1 << 17)));
    }

    // 변이 테스트 보강: 버퍼 밖 위치의 주입은 무시한다 (버퍼 밖에 쓰면 안 된다)
    #[test]
    fn out_of_range_injection_is_ignored() {
        let words = 8 * 1024 * 1024 / 8;
        let inj = MemInject { pass: 0, word: words, bit: 0 };
        let out = run(&MemConfig { mb: 8, duration: Duration::from_millis(300), inject: Some(inj) });
        assert!(out.error.is_none(), "{:?}", out.error);
    }

    // 변이 테스트 보강: 패턴 값은 정답표와 같아야 한다
    #[test]
    fn pattern_values_are_known() {
        let solid = [0x5555_5555_5555_5555, 0xAAAA_AAAA_AAAA_AAAA, 0, u64::MAX];
        for pass in [0, 1, 2, 3, 5, 6, 7, 8] {
            assert_eq!(value(pattern_for(pass), 1000), solid[pass as usize % 5], "pass={pass}");
        }
        assert_eq!(value(pattern_for(4), 0), 0xD1B5_4A32_D192_ED07);
        assert_eq!(value(pattern_for(4), 1000), 0xD906_36AB_EB66_5F0F);
        assert_eq!(value(pattern_for(9), 1), 0x4F82_338B_AED8_911F);
    }
}
