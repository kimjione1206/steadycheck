# verify

External checks that steadycheck catches real faults: faults are injected from outside the unmodified release binary.

- `memflip/`: a separate Windows tool that flips (`once`) or pins (`stuck0`/`stuck1`) one bit in the largest read-write private region of a running process, or only reads it (`watch`, control).
- `run-memflip.ps1`: starts `steadycheck mem --mb 512 --seconds 20 --threads 4`, runs memflip against it, and compares the reported error with the injected bit and address.
- CI: `.github/workflows/verify.yml` (job `memflip`) runs on pushes to `verify-*` branches and on manual dispatch, with a fixed random seed.
- Pass criteria: every stuck-bit run is caught at the injected bit with one consistent buffer offset, every caught one-shot flip matches the same way, and every control run passes.
