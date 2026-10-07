# Mutation testing results

Mutation testing changes the code in one small way at a time (an operator swapped, a condition flipped, a function body replaced; each change is a "mutant") and checks that at least one test fails. A mutant no test notices points either to a gap in the tests or to a change that makes no difference at all. This page lists every mutant that was not caught and why. How the run works is described in [How it works](HOW-IT-WORKS.md).

**Scope:** `src/mem.rs` (memory test), `src/memsim.rs` (faulty-memory simulator), `src/report.rs` (verdict), `src/cli.rs` (command line), `src/whea.rs` (Windows hardware-error log). The CPU and core-to-core code is not included.
**Platform:** Windows x86-64, release build — the build that ships.
**Runs:** one full run, then, after tests were added for 14 mutants that had been missed, a re-run of the mutants on those lines.

| Result | Count |
|---|---|
| Caught (including 3 that ran into the time limit after tests had already failed) | 756 |
| Cannot be caught — the change makes no observable difference (see below): 22 equivalent, 1 unobservable memory fence, 2 timing-only | 25 |
| Did not build | 26 |
| **Total** | **807** |

Every mutant that could be caught was caught.

## Mutants that cannot be caught

Line numbers change as the code changes, so mutants are named by file, function and change.

| File · function | Change | Why it makes no difference |
|---|---|---|
| cli.rs · `parse` | guard `val == "auto"` → `false` in the non-Windows `--mb auto` arm | On Windows the arm before it already takes every `auto`, so this guard is only reached when it is false anyway. (Windows only; on other platforms a test catches it.) |
| memsim.rs · `set` | `\|` → `^` | The two values have no bits in common, so OR and XOR give the same result. |
| memsim.rs · `at` | `%` → `+` | The result is used only as a shift amount, which the release build masks to 6 bits: (q + 64) mod 64 = q mod 64. (Release build.) |
| memsim.rs · `read` | `\|` → `^` | No bits in common, as above. |
| memsim.rs · `write_line` | `\|` → `^` | No bits in common, as above. |
| mem.rs · `Buf::fence` | body removed | The store fence only orders when other cores see this core's writes; the gap it closes is a hardware race shorter than the hand-off between workers. No deterministic test can show it. |
| mem.rs · `run_element` | fast path for a write-only sweep removed | The general path gives identical results; it is only slower. A test checks that both paths agree for every sweep shape. |
| mem.rs · `run_element` | fast path for a read-only sweep removed | Same as above. |
| mem.rs · `run_element` | fast path for read-then-write sweeps disabled | Same as above. |
| mem.rs · `run_element` | `%` → `+` in the stripe pattern for stripes 6–8 | Only bits 0–2 of the index are used; adding 8 changes bit 3 and above only. |
| mem.rs · `line_skip` | `%` → `+` | Wrap-around arithmetic gives the same result because 64 divides 2^64. (Release build.) |
| mem.rs · `e_sweep` | `-` → `+` in the last-line length | Block bounds are always whole lines apart, so both forms give 8. |
| mem.rs · `worker` | `&&` → `\|\|` when choosing the next D/E stage | Changes only which worker's clock sets the 60:40 time split; all workers still read one shared value. (Timing only.) |
| mem.rs · `worker` | `-` → `+` in a test-only line | pass − STAGES and pass + STAGES differ by 2 · STAGES, an even number, so their parity is the same. Not in the shipped program. |
| mem.rs · `chunk_cells` | `&&` → `\|\|` in the chunk-range check | A word outside the chunk never matches a word the chunk visits (a word before the chunk wraps to a huge index), so the fault hook never fires either way. (Release build.) |
| mem.rs · `chunk_cells` | `<` → `<=` in the chunk-range check | The word right after the chunk is never visited. |
| mem.rs · `chunk_cells` | `%` → `+` in a simulated fault's bit shift (coupling) | Shift amount masked to 6 bits. (Release build.) |
| mem.rs · `chunk_cells` | `%` → `+` in a simulated fault's bit shift (busy-only) | Same as above. (Release build.) |
| mem.rs · `flip_if` | `%` → `+` in the injected bit's shift | Same as above. (Release build.) |
| mem.rs · `stage` | `e > 0` → `e >= 0` for the per-sweep stop check | Adds one extra, synchronised stop check right after an identical one. (Timing only.) |
| mem.rs · `stage` | `*` → `/` when counting bytes checked | Every sweep that can report an error has exactly one read, and x · 1 = x / 1. |
| mem.rs · `stage` | `<` → `<=` for "still in the base set" | When this line runs at that boundary, the base set is already complete for every worker, and the values it updates are no longer used. |
| report.rs · `cpu_brand` | `<` → `<=` | Differs only on a CPU that reports a specific old limit; no x86-64 CPU does. (x86-64 hardware.) |
| report.rs · `cpu_brand` | `<` → `==` | Differs only on a CPU that reports this old limit or lower; no x86-64 CPU does. (x86-64 hardware.) |
| whea.rs · `query` (non-Windows version) | body replaced | This version is not part of the Windows build at all. (Windows only; on other platforms a test covers it.) |

"Release build", "Windows only" and "x86-64 hardware" mean the reason holds on the platform that ships (Windows x86-64, release build), not necessarily elsewhere.
