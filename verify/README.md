# verify

External checks that steadycheck catches real faults: faults are injected from outside the unmodified release binary.

- `memflip/`: a separate Windows tool that flips (`once`) or pins (`stuck0`/`stuck1`) one bit in the largest read-write private region of a running process, or only reads it (`watch`, control).
- `run-memflip.ps1`: starts `steadycheck mem --mb 512 --seconds 20 --threads 4`, runs memflip against it, and compares the reported error with the injected bit and address.
- CI: `.github/workflows/verify.yml` (job `memflip`) runs on pushes to `verify-*` branches and on manual dispatch, with a fixed random seed.
- Pass criteria: every stuck-bit run is caught at the injected bit and direction with one consistent buffer offset, each of the 4 workers catches at least one, every caught one-shot flip (at least one) matches the same way, and every control run passes.

- `drfault/`: a DynamoRIO client that, inside one thread (with `-thread worker`, the first non-main thread to reach N; with `main`, the main thread), XORs one bit of lane 0 of the destination register after every N-th execution of chosen instructions (`-ops vfmadd231pd`, `-ops vpmuludq`, `-every N`, `-bit B`, `-thread worker|main`, `-log`), imitating a single core that computes wrong.
- `run-drfault.ps1`: runs `steadycheck cpu --isa avx2 --kernel fma|wide --threads 4 --iters 4096 --seconds 20` under `drrun` with the client and compares the verdict and failing kernel with the injection log.
- The CPU injection job (`drfault`) runs on Linux because DynamoRIO on Windows x64 does not preserve ymm6–15, which breaks steadycheck's AVX2 results even without a client. Threads are not pinned on Linux, so the failing hardware CPU is not checked there; pass criteria are: control run PASS with no injections, and every injecting run FAIL in the injected kernel with all injections from one thread.
