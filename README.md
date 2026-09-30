# steadycheck

CPU and memory stability checker for Windows x86-64. Every result is compared bit-for-bit
against a known answer; any mismatch is a failure.

    steadycheck cpu --seconds 60
    steadycheck mem --mb 1024 --seconds 60
    steadycheck all --seconds 60

`all` runs CPU then RAM, each for `--seconds` (plus a few seconds to build the answer table).
Other options: `--threads N  --isa auto|scalar|avx2|avx512  --kernel mix|chain|wide|fma|fma32  --pattern steady|pulse  --iters N  --mb N`.
`mix` (default) rotates four kernels block by block: `chain` (one dependent chain), `wide` (32 independent integer lanes), `fma` (32 double-precision fused multiply-add lanes) and `fma32` (32 single-precision lanes). `pulse` switches the load on and off every 250 ms on all cores at once.
When `--isa` is omitted on a CPU with AVX-512, blocks alternate between the AVX-512 and AVX2 paths (both must give identical results), and the failing path is reported as `error.isa`.

Output: JSON on stdout. Exit code 0 PASS, 1 FAIL, 2 unsupported environment, 3 usage error.

Copyright (c) 2026 kimjione1206. All rights reserved. Source is visible for reference only.
