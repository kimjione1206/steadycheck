# steadycheck

CPU and memory stability checker for Windows x86-64. Every result is compared bit-for-bit
against a known answer; any mismatch is a failure.

    steadycheck cpu --seconds 60
    steadycheck mem --mb 1024 --seconds 60
    steadycheck all --seconds 60

`all` runs CPU then RAM, each for `--seconds` (plus a few seconds to build the answer table).
Other options: `--threads N  --isa auto|scalar|avx2|avx512  --iters N  --mb N`

Output: JSON on stdout. Exit code 0 PASS, 1 FAIL, 2 unsupported environment, 3 usage error.

Copyright (c) 2026 kimjione1206. All rights reserved. Source is visible for reference only.
