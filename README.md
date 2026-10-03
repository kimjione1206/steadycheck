# steadycheck

CPU and memory stability checker for Windows x86-64. Every result is compared bit-for-bit
against a known answer; any mismatch is a failure.

    steadycheck cpu --seconds 60
    steadycheck share --seconds 60
    steadycheck mem --mb 1024 --seconds 60
    steadycheck all --seconds 60

`all` runs cpu → share → mem, each for the full `--seconds`, so it takes about 3 × `--seconds` (plus a few seconds to build the answer table); it stops at the first failure.
The RAM test splits the `--mb` buffer across `--threads` workers (one per logical CPU by default, never more than the logical CPUs) that run at the same time, so the memory controller sees full bandwidth. Each pass writes a pattern, then reads it back while writing its complement in the same sweep, then reads the complement. Patterns rotate through 0x55…, 0xAA…, all zeros, all ones, an address hash and a pseudo-random sequence with a new seed every pass. Use `--threads 1` for a single worker. Every worker must finish at least one pass or the run fails. In the JSON, `mem.passes` is the sum of per-worker chunk passes and `mem.min_thread_passes` is the lowest count of any worker.
Other options: `--threads N  --isa auto|scalar|avx2|avx512  --kernel mix|chain|wide|fma|fma32|lz  --pattern steady|pulse|cycle  --iters N  --mb N`.
`mix` (default) rotates five kernels block by block: `chain` (one dependent chain), `wide` (32 independent integer lanes), `fma` (32 double-precision fused multiply-add lanes), `fma32` (32 single-precision lanes) and `lz` (a deterministic decompressor-shaped kernel: byte stores, overlapping copies, `rep movsb`, BMI bit extraction; every written byte is read back into a checksum). `pulse` switches the load on and off every 250 ms on all cores at once. `cycle` runs one core at a time while the others rest, so each core can reach its highest boost clock and wakes up from idle on every turn. Every worker thread (one per logical CPU by default) must complete at least one block or the run fails, so `cycle` needs at least 0.5 s per thread (`--seconds` ≥ threads / 2); a shorter run exits with code 3. For the highest clocks use a light load, e.g. `steadycheck cpu --pattern cycle --kernel chain --isa scalar --seconds 120`.
When `--isa` is omitted on a CPU with AVX-512, blocks alternate between the AVX-512 and AVX2 paths (both must give identical results), and the failing path is reported as `error.isa`.
`share` tests how cores talk to each other: pinned workers (one per logical CPU by default, never more than the logical CPUs) pass cache-line messages around a ring and count with an atomic add; every received word is compared with its known value, and the shared count must equal the number of messages. Every worker must receive at least one message or the run fails. The workers spin-wait for each other, so on a heavily loaded machine a worker can be starved and the run counts as FAIL; run it on an otherwise idle PC. In the JSON, `share.min_thread_messages` is the lowest count of any worker and `share.messages_per_sec` the total rate.

Output: JSON on stdout. Exit code 0 PASS, 1 FAIL, 2 unsupported environment, 3 usage error.

Copyright (c) 2026 kimjione1206. All rights reserved. Source is visible for reference only.
