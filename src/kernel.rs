//! 계산 커널: 정답을 아는 정수 계산. 스칼라가 기준이고 SIMD 는 비트 단위로 같아야 한다.

pub const LANES: usize = 8;
pub const ITERS_PER_BLOCK: u64 = 1 << 24;
// x 의 하위 32비트에 곱하는 32비트 상수 (AVX2 의 mul_epu32 와 같은 연산)
const MUL: u64 = 0x9E37_79B9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Isa {
    Scalar,
    Avx2,
    Avx512,
}

impl Isa {
    pub fn parse(s: &str) -> Option<Isa> {
        match s {
            "scalar" => Some(Isa::Scalar),
            "avx2" => Some(Isa::Avx2),
            "avx512" => Some(Isa::Avx512),
            _ => None,
        }
    }

    pub fn supported(self) -> bool {
        match self {
            Isa::Scalar => true,
            #[cfg(target_arch = "x86_64")]
            // fma 커널이 AVX2 경로에서 FMA 명령도, lz 커널이 BMI1·BMI2 도 쓴다
            Isa::Avx2 => {
                is_x86_feature_detected!("avx2")
                    && is_x86_feature_detected!("fma")
                    && is_x86_feature_detected!("bmi1")
                    && is_x86_feature_detected!("bmi2")
            }
            #[cfg(target_arch = "x86_64")]
            Isa::Avx512 => {
                is_x86_feature_detected!("avx512f") && is_x86_feature_detected!("bmi1") && is_x86_feature_detected!("bmi2")
            }
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    pub fn best() -> Isa {
        if Isa::Avx512.supported() {
            Isa::Avx512
        } else if Isa::Avx2.supported() {
            Isa::Avx2
        } else {
            Isa::Scalar
        }
    }
}

/// 검출 능력 채점용: 계산 시작 전에 lane 의 bit 하나를 뒤집는다.
#[derive(Clone, Copy, Debug)]
pub struct Flip {
    pub lane: usize,
    pub bit: u32,
}

/// 계산 방식. chain: 한 줄 사슬(기존), wide: 32줄 동시 정수, fma: 32줄 곱셈-덧셈, fma32: 32줄 단정밀도 곱셈-덧셈, lz: 압축 해제 모양 바이트 복사.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kernel {
    Chain,
    Wide,
    Fma,
    Fma32,
    Lz,
}

impl Kernel {
    pub const ALL: [Kernel; 5] = [Kernel::Chain, Kernel::Wide, Kernel::Fma, Kernel::Fma32, Kernel::Lz];

    pub fn parse(s: &str) -> Option<Kernel> {
        match s {
            "chain" => Some(Kernel::Chain),
            "wide" => Some(Kernel::Wide),
            "fma" => Some(Kernel::Fma),
            "fma32" => Some(Kernel::Fma32),
            "lz" => Some(Kernel::Lz),
            _ => None,
        }
    }

    /// 블록 한 번 반복에 동시에 계산하는 줄 수
    pub fn lanes(self) -> u64 {
        match self {
            Kernel::Chain => LANES as u64,
            Kernel::Wide => crate::wide::LANES as u64,
            Kernel::Fma => crate::fma::LANES as u64,
            Kernel::Fma32 => crate::fma32::LANES as u64,
            Kernel::Lz => 1,
        }
    }

    /// 블록 하나가 수십 ms 가 되도록 고른 기본 반복 수
    pub fn default_iters(self) -> u64 {
        match self {
            Kernel::Chain => ITERS_PER_BLOCK,
            Kernel::Wide => 1 << 22,
            Kernel::Fma => 1 << 22,
            Kernel::Fma32 => 1 << 22,
            Kernel::Lz => 1 << 21,
        }
    }
}

pub(crate) fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn seed_state(seed: u64) -> ([u64; LANES], [u64; LANES]) {
    let mut x = [0u64; LANES];
    for (i, v) in x.iter_mut().enumerate() {
        *v = splitmix64(seed.wrapping_mul(LANES as u64).wrapping_add(i as u64)) | 1;
    }
    (x, [0u64; LANES])
}

fn fold(x: &[u64; LANES], y: &[u64; LANES]) -> u64 {
    let mut h = 0u64;
    for i in 0..LANES {
        h = h.rotate_left(7) ^ x[i] ^ y[i].rotate_left(29);
    }
    h
}

fn run_scalar(mut x: [u64; LANES], mut y: [u64; LANES], iters: u64) -> u64 {
    for _ in 0..iters {
        for i in 0..LANES {
            let mut v = x[i];
            v ^= v << 13;
            v ^= v >> 7;
            v ^= v << 17;
            x[i] = v;
            y[i] = y[i].wrapping_add((v & 0xFFFF_FFFF) * MUL);
        }
    }
    fold(&x, &y)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn run_avx2(x: [u64; LANES], y: [u64; LANES], iters: u64) -> u64 {
    use std::arch::x86_64::*;
    let m = _mm256_set1_epi64x(MUL as i64);
    let mut xa = _mm256_loadu_si256(x.as_ptr() as *const __m256i);
    let mut xb = _mm256_loadu_si256(x.as_ptr().add(4) as *const __m256i);
    let mut ya = _mm256_loadu_si256(y.as_ptr() as *const __m256i);
    let mut yb = _mm256_loadu_si256(y.as_ptr().add(4) as *const __m256i);
    for _ in 0..iters {
        xa = _mm256_xor_si256(xa, _mm256_slli_epi64(xa, 13));
        xb = _mm256_xor_si256(xb, _mm256_slli_epi64(xb, 13));
        xa = _mm256_xor_si256(xa, _mm256_srli_epi64(xa, 7));
        xb = _mm256_xor_si256(xb, _mm256_srli_epi64(xb, 7));
        xa = _mm256_xor_si256(xa, _mm256_slli_epi64(xa, 17));
        xb = _mm256_xor_si256(xb, _mm256_slli_epi64(xb, 17));
        ya = _mm256_add_epi64(ya, _mm256_mul_epu32(xa, m));
        yb = _mm256_add_epi64(yb, _mm256_mul_epu32(xb, m));
    }
    let mut ox = [0u64; LANES];
    let mut oy = [0u64; LANES];
    _mm256_storeu_si256(ox.as_mut_ptr() as *mut __m256i, xa);
    _mm256_storeu_si256(ox.as_mut_ptr().add(4) as *mut __m256i, xb);
    _mm256_storeu_si256(oy.as_mut_ptr() as *mut __m256i, ya);
    _mm256_storeu_si256(oy.as_mut_ptr().add(4) as *mut __m256i, yb);
    fold(&ox, &oy)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn run_avx512(x: [u64; LANES], y: [u64; LANES], iters: u64) -> u64 {
    use std::arch::x86_64::*;
    let m = _mm512_set1_epi64(MUL as i64);
    let mut xv = _mm512_loadu_si512(x.as_ptr() as *const __m512i);
    let mut yv = _mm512_loadu_si512(y.as_ptr() as *const __m512i);
    for _ in 0..iters {
        xv = _mm512_xor_si512(xv, _mm512_slli_epi64::<13>(xv));
        xv = _mm512_xor_si512(xv, _mm512_srli_epi64::<7>(xv));
        xv = _mm512_xor_si512(xv, _mm512_slli_epi64::<17>(xv));
        yv = _mm512_add_epi64(yv, _mm512_mul_epu32(xv, m));
    }
    let mut ox = [0u64; LANES];
    let mut oy = [0u64; LANES];
    _mm512_storeu_si512(ox.as_mut_ptr() as *mut __m512i, xv);
    _mm512_storeu_si512(oy.as_mut_ptr() as *mut __m512i, yv);
    fold(&ox, &oy)
}

/// 시드 하나로 블록 하나를 계산해 64비트 요약값을 돌려준다.
pub fn run_block(kernel: Kernel, isa: Isa, seed: u64, iters: u64, flip: Option<Flip>) -> u64 {
    assert!(isa.supported(), "{isa:?} 미지원");
    match kernel {
        Kernel::Chain => chain_block(isa, seed, iters, flip),
        Kernel::Wide => crate::wide::run(isa, seed, iters, flip),
        Kernel::Fma => crate::fma::run(isa, seed, iters, flip),
        Kernel::Fma32 => crate::fma32::run(isa, seed, iters, flip),
        Kernel::Lz => crate::lz::run(isa, seed, iters, flip),
    }
}

fn chain_block(isa: Isa, seed: u64, iters: u64, flip: Option<Flip>) -> u64 {
    let (mut x, y) = seed_state(seed);
    if let Some(f) = flip {
        x[f.lane % LANES] ^= 1u64 << (f.bit % 64);
    }
    match isa {
        Isa::Scalar => run_scalar(x, y, iters),
        #[cfg(target_arch = "x86_64")]
        Isa::Avx2 => unsafe { run_avx2(x, y, iters) },
        #[cfg(target_arch = "x86_64")]
        Isa::Avx512 => unsafe { run_avx512(x, y, iters) },
        #[allow(unreachable_patterns)]
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 정답값: 2026-09-30 맥(ARM 네이티브)과 Rosetta x86 에서 같은 값으로 실측
    #[test]
    fn known_answers() {
        assert_eq!(run_block(Kernel::Chain, Isa::Scalar, 0, 1000, None), 0x1F56_BA5F_3651_85D2);
        assert_eq!(run_block(Kernel::Chain, Isa::Scalar, 1, 1000, None), 0x8DC6_4E16_440C_9F32);
        assert_eq!(run_block(Kernel::Chain, Isa::Scalar, 42, 1000, None), 0xE0D6_4897_26E6_C539);
    }

    #[test]
    fn simd_matches_scalar() {
        for isa in [Isa::Avx2, Isa::Avx512] {
            if !isa.supported() {
                eprintln!("{isa:?} 미지원 — 건너뜀");
                continue;
            }
            for seed in 0..32 {
                for iters in [1, 1000, 4097] {
                    assert_eq!(
                        run_block(Kernel::Chain, isa, seed, iters, None),
                        run_block(Kernel::Chain, Isa::Scalar, seed, iters, None),
                        "{isa:?} seed={seed} iters={iters}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_flip_changes_result() {
        let clean = run_block(Kernel::Chain, Isa::Scalar, 7, 1000, None);
        for lane in 0..LANES {
            for bit in [0, 1, 31, 32, 63] {
                let hit = run_block(Kernel::Chain, Isa::Scalar, 7, 1000, Some(Flip { lane, bit }));
                assert_ne!(hit, clean, "lane={lane} bit={bit}");
            }
        }
    }

    // 변이 테스트 보강: 주입은 정확히 그 lane 의 그 bit 하나만 뒤집어야 한다
    #[test]
    fn flip_changes_exactly_one_bit() {
        let (mut x, y) = seed_state(7);
        x[3] ^= 1 << 40;
        assert_eq!(run_block(Kernel::Chain, Isa::Scalar, 7, 1000, Some(Flip { lane: 3, bit: 40 })), run_scalar(x, y, 1000));
    }

    #[test]
    fn parse_and_best() {
        assert_eq!(Isa::parse("scalar"), Some(Isa::Scalar));
        assert_eq!(Isa::parse("avx2"), Some(Isa::Avx2));
        assert_eq!(Isa::parse("avx512"), Some(Isa::Avx512));
        assert_eq!(Isa::parse("sse"), None);
        assert!(Isa::best().supported());
    }

    #[test]
    fn kernel_parse_and_shape() {
        assert_eq!(Kernel::parse("chain"), Some(Kernel::Chain));
        assert_eq!(Kernel::parse("wide"), Some(Kernel::Wide));
        assert_eq!(Kernel::parse("fma"), Some(Kernel::Fma));
        assert_eq!(Kernel::parse("fma32"), Some(Kernel::Fma32));
        assert_eq!(Kernel::parse("lz"), Some(Kernel::Lz));
        assert_eq!(Kernel::parse("mix"), None);
        assert_eq!(Kernel::ALL, [Kernel::Chain, Kernel::Wide, Kernel::Fma, Kernel::Fma32, Kernel::Lz]);
        assert_eq!(Kernel::ALL.map(Kernel::lanes), [8, 32, 32, 32, 1]);
        assert_eq!(Kernel::ALL.map(Kernel::default_iters), [ITERS_PER_BLOCK, 1 << 22, 1 << 22, 1 << 22, 1 << 21]);
        assert_eq!(Kernel::Chain.default_iters(), 1 << 24);
        for k in [Kernel::Wide, Kernel::Fma, Kernel::Fma32, Kernel::Lz] {
            let direct = match k {
                Kernel::Wide => crate::wide::run(Isa::Scalar, 0, 1000, None),
                Kernel::Fma => crate::fma::run(Isa::Scalar, 0, 1000, None),
                Kernel::Fma32 => crate::fma32::run(Isa::Scalar, 0, 1000, None),
                _ => crate::lz::run(Isa::Scalar, 0, 1000, None),
            };
            assert_eq!(run_block(k, Isa::Scalar, 0, 1000, None), direct, "{k:?}");
        }
    }
}
