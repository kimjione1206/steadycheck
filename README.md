# steadycheck

**A free, open-source CPU and RAM stability tester for Windows. One exe, no install, and a plain PASS or FAIL.**

[Download the latest release](https://github.com/kimjione1206/steadycheck/releases/latest) · [How to test and read the result](docs/TESTING.md) · [한국어](README.ko.md)

- **Exact answers, not "looks fine".** Every CPU core computes the same known workloads and every memory word is read back and compared bit for bit. One wrong bit is a FAIL.
- **Tells you where a RAM error is.** It says whether the wrong value was *stored* in memory or went wrong *while being read back*, and which of the 512 bit positions in the 64-byte cache line failed. That points toward a bad cell, a data line, or settings that are too tight.
- **The tester itself is tested.** Its memory test is run against a simulated faulty memory, against bits flipped from outside while it runs, and against deliberately broken versions of its own code (details [below](#how-far-it-is-proven)).

> **Status:** command line only, no window. Checked so far with simulated and injected faults, **not yet on real unstable or faulty PCs**. If you have a setting you know is unstable, [your result would help](#help-test-on-real-hardware).

## Quick start

1. Download `steadycheck-<version>-windows-x86_64.exe` from [Releases](https://github.com/kimjione1206/steadycheck/releases/latest).
2. In the download folder, right-click empty space → **Open in Terminal** (Windows 10: Shift + right-click → **Open PowerShell window here**).
3. Close other programs and run one of these (use the version you downloaded — type `.\steadycheck` and press **Tab** to fill in the file name):

Check the whole PC — CPU, core-to-core, RAM (about 15 minutes):

    .\steadycheck-0.6.0-windows-x86_64.exe all --mb auto --seconds 300 > result.json

Check RAM on a setting you suspect, recording up to 20 errors (about 10 minutes):

    .\steadycheck-0.6.0-windows-x86_64.exe mem --mb auto --seconds 600 --keep-going 20 > result.json

For an end-of-line check, add `--require-complete` so a run that was too short ends as INCOMPLETE instead of PASS.

The last line in the window is `판정: PASS`, `판정: FAIL` or `판정: INCOMPLETE` (`판정` = verdict). The full result is in `result.json`.

## Reading the result

| Verdict | Exit code | What it means | What to do |
|---|---|---|---|
| PASS | 0 | No wrong value was found in this run. | It is not a guarantee. For RAM, check that `mem.rounds_d` is at least 4; if not, run longer. |
| FAIL | 1 | A wrong value was caught. | Back off the overclock or XMP/EXPO, or test one memory module at a time. |
| INCOMPLETE | 4 | No error, but the memory test did not get far enough (only with `--require-complete`). | Run again with a larger `--seconds`. |

Exit code 2 means the PC does not support what was asked, 3 a typing mistake in the command.

A memory error looks like this (abridged output of `steadycheck mem --mb 64 --seconds 10 --inject-mem 1:500`, which flips one bit on purpose):

```json
{
  "verdict": "FAIL",
  "injected": true,
  "mem": {
    "error": {
      "thread": 0,
      "stage": "B",
      "pattern": "solid",
      "offset_bytes": 4000,
      "expected": "0x0000000000000000",
      "actual":   "0x0000000000000001",
      "reread":   "0x0000000000000001",
      "kind": "stored",
      "line_bits": [256]
    }
  }
}
```

- `kind: "stored"`: reading the word again straight from memory still gives the wrong value, so the wrong value is in memory. `kind: "read"` would mean memory now holds the right value and the error happened on the way back.
- `line_bits`: where in the 64-byte line the wrong bits are (0–511). The same position failing at different addresses points to a data line; the same address failing again points to a cell; scattered positions point to signal integrity or settings that are too tight.
- With `--keep-going N` the memory test does not stop at the first error: `mem.errors_total` and the list `mem.errors` show how many errors appeared and when.
- On Windows the result also counts the Windows hardware error log entries (WHEA) written during the run (`whea`) and shows what share of the PC's memory was tested (`mem.tested_percent`). Neither changes the verdict.

## What it tests

| Mode | What it does |
|---|---|
| `cpu` | Every logical CPU runs the same integer, floating-point (FMA, AVX2, AVX-512) and decompression-shaped workloads; results must match a known answer exactly. |
| `share` | Cores pass messages to each other and add to shared counters; every message and the final total must be exact. |
| `mem` | Fills the memory buffer with known patterns and reads them back, using a classic memory-test sequence (March C-), nine cache-line stripe patterns, randomized rounds, and fast read/write switching. |
| `all` | `cpu`, then `share`, then `mem`, each for `--seconds`; stops at the first failure. |

The full description of every stage, option and output field is in [How it works](docs/HOW-IT-WORKS.md).

## How far it is proven

- **Simulated faulty memory:** the real memory-test code is run on a simulated memory with one fault at a time; given enough run time (`mem.rounds_d` of at least 4) it catches every fault in that list (stuck bits, bits that cannot change, address faults, coupling between words, data line shorts). This is coverage within the simulator, not a promise about every real-world defect — see the [coverage table and its limits](docs/HOW-IT-WORKS.md).
- **Faults injected from outside:** a separate program sticks or flips bits in steadycheck's memory while it runs; steadycheck catches stuck bits and reports the right address and bit (a single short flip can be overwritten before it is read back, so not every one is caught).
- **Mutation testing:** the code is broken on purpose in hundreds of small ways to check that the tests notice (624 of 670 caught in the run for v0.5.0).
- **Not yet:** real unstable or faulty hardware.

**Limits.** DDR5 corrects single-bit errors inside each chip (on-die ECC) without reporting them, so those are invisible to any software test. RowHammer is out of scope. It runs inside Windows, so memory used by Windows and other programs is not tested. A PASS is not a guarantee.

## Download and safety

Get the exe and `SHA256SUMS.txt` from [Releases](https://github.com/kimjione1206/steadycheck/releases/latest). Both are built by GitHub Actions from this repository.

- **Heavy load:** CPU and memory run at full load, so the PC gets hot and loud, and an unstable PC may freeze or restart. Save your work first; on a setting you made unstable on purpose, back up important files.
- **Unknown publisher:** the exe is not code-signed, so Windows SmartScreen warns. With Windows 11 Smart App Control turned on, unsigned programs are blocked and cannot be started.

<details>
<summary>Check that the file is genuine</summary>

- Fingerprint: `certutil -hashfile steadycheck-<version>-windows-x86_64.exe SHA256` must print the same value as `SHA256SUMS.txt`.
- Where it was built: `gh attestation verify steadycheck-<version>-windows-x86_64.exe -R kimjione1206/steadycheck --source-ref refs/tags/v<version> --signer-workflow kimjione1206/steadycheck/.github/workflows/release.yml` (needs the GitHub CLI and `gh auth login`). It passes only for a file built from that release tag.

</details>

## Help test on real hardware

So far steadycheck has been checked against simulated faults, faults injected from outside and mutation testing, not yet on real unstable or faulty PCs.
If you can run it on a setting you know is unstable, or on a PC you know is stable, see the [testing guide](docs/TESTING.md) ([한국어](docs/TESTING.ko.md)).
It explains the safety notes, how to run and read the result, and which issue form to use; `tools/collect-info.ps1` gathers the hardware details without serial numbers or names.

Copyright (c) 2026 kimjione1206. Licensed under the MIT License — see [LICENSE](LICENSE).
