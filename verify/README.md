# verify

External checks that steadycheck catches real faults: faults are injected from outside the unmodified release binary.

- `memflip/`: a separate Windows tool that flips (`once`) or pins (`stuck0`/`stuck1`) one bit in the largest read-write private region of a running process, or only reads it (`watch`, control).
- `run-memflip.ps1`: starts `steadycheck mem --mb 512 --seconds 20 --threads 4`, runs memflip against it, and compares the reported error with the injected bit and address.
- CI: `.github/workflows/verify.yml` (job `memflip`) runs on pushes to `verify-*` branches and on manual dispatch, with a fixed random seed.
- Pass criteria: every stuck-bit run is caught at the injected bit and direction with one consistent buffer offset, each of the 4 workers catches at least one, every caught one-shot flip (at least one) matches the same way, and every control run passes.

- `drfault/`: a DynamoRIO client that, in selected threads (`-thread worker`: the first non-main thread to reach the condition; `main`: the main thread; `main+worker`: both), XORs one bit of lane 0 of the destination register after every N-th execution of chosen instructions, or whenever the low 12 bits of the result equal K (`-ops vfmadd231pd`, `-ops vpmuludq`, `-every N` or `-match K`, `-after N`, `-bit B`, `-thread worker|main|main+worker`, `-mask0`, `-log`), imitating a single core that computes wrong.
- `run-drfault.ps1`: runs `steadycheck cpu --isa avx2 --kernel fma|wide --threads 4 --iters 4096 --seconds 20` under `drrun` with the client and compares the verdict and failing kernel with the injection log.
- The CPU injection job (`drfault`) runs on Linux because DynamoRIO on Windows x64 does not preserve ymm6–15, which breaks steadycheck's AVX2 results even without a client. Threads are not pinned on Linux, so the failing hardware CPU is not checked there.
- Pass criteria: the controls pass (fma and wide with no injections, and a register round-trip that XORs 0 via `-mask0`), and every injecting run is FAIL with a stable golden table, at least one injection, and all injections from one thread. Each run uses a single kernel, so the reported kernel matching it is only a consistency check.
- E3 (recorded, not gated): faults in the thread that builds the golden table (`-thread main` or `main+worker`, `-every N` or `-match K` = flip whenever the low 12 bits of the result equal K). Some rows are caught by the golden self-check; two rows use `-after N` to skip the self-check part (`--iters 16384`), so a consistent fault is baked into an accepted golden table. A PASS there is flagged as a warning, a candidate design weakness.
