# Testing steadycheck on real hardware

[한국어](TESTING.ko.md)

## Before you start

- **Smart App Control:** open Windows Security → App & browser control → Smart App Control settings. If it is **On**, Windows blocks the unsigned steadycheck exe outright (there is no "Run anyway"), so this PC cannot be used for testing for now — please use another PC if you have one. If it is Off, or the setting does not exist (Windows 10), you can go ahead.
- **The window stays quiet while it runs.** Nothing is printed until the test ends; then the last line, `판정: PASS`, `판정: FAIL` or `판정: INCOMPLETE` (`판정` = verdict), appears. That is normal — do not close the window.
- **Time:** about 30 minutes in all — 5–10 minutes to download and check, about 10 minutes for the memory test (about 15 for the whole-PC test), and a few minutes to send the result.

## 1. What we are asking

Two questions need real PCs:
- Does steadycheck **catch** real instability — an unstable overclock, too-tight memory timings, too-low voltage, a faulty part?
- Does it stay **quiet** (PASS) on a PC that is stable?

Results from both DDR4 and DDR5 PCs are wanted: `tools/collect-info.ps1` now prints the memory type, which you then pick in the **Memory type** field of the issue form.

What has been checked so far, all on GitHub's servers:
- **Simulated faults:** the memory base set runs on a simulated memory holding one fault at a time from a fixed catalogue (see the table in [How it works](HOW-IT-WORKS.md)).
- **Faults injected from outside:** single bits flipped or pinned in the memory of the unmodified program while it runs, and single bits flipped in CPU results (`verify/`).
- **Mutation testing:** small deliberate breaks in the code, to confirm the tests notice them.

Not checked yet: real faulty or unstable hardware — that is what your result adds. Known limits: DDR5 corrects single-bit errors inside each chip without reporting them, RowHammer is not tested, and faults that depend on temperature or timing only get a chance to show in longer runs.

## 2. Safety

- steadycheck puts a high load on the CPU and memory: the PC gets hot and the fans get loud.
- Laptops and PCs with weak cooling: watch the temperatures and stop with Ctrl+C if they get too high.
- Save and close your work first. An unstable setting can freeze or restart the PC.
- Back up important files, and use a separate test installation of Windows if you can: unstable memory can corrupt files on disk and Windows itself.
- Do not raise voltages beyond what you normally consider safe. Tighter timings, a higher frequency at the same voltage or a lower voltage are enough to make a setting unstable.
- You run it at your own risk. steadycheck is provided "AS IS", without warranty of any kind (MIT License, see [LICENSE](../LICENSE)).

## 3. Download and check

Put the downloaded files in one folder. To open PowerShell there, right-click an empty space in that folder in File Explorer and choose "Open in Terminal" (on Windows 10, hold Shift while right-clicking and choose "Open PowerShell window here"); type the commands on this page in that window.

In the commands, replace `<version>` with the number in the name of the file you downloaded. For example, for `steadycheck-0.6.0-windows-x86_64.exe` type `.\steadycheck-0.6.0-windows-x86_64.exe mem --mb auto --seconds 600 --require-complete --keep-going 20 > result.json` (and `refs/tags/v0.6.0` in step 3).

1. From the [Releases](https://github.com/kimjione1206/steadycheck/releases) page, get `steadycheck-<version>-windows-x86_64.exe` and `SHA256SUMS.txt`.
2. Check the fingerprint — this one line compares the file with `SHA256SUMS.txt` and prints `True` if they match, `False` if not (do not run a file that gives `False`):
   `(Get-FileHash .\steadycheck-<version>-windows-x86_64.exe -Algorithm SHA256).Hash -eq (-split (Get-Content .\SHA256SUMS.txt))[0]`
3. Optional — check where it was built. This needs a GitHub account and the GitHub CLI (sign in once with `gh auth login` first); skip it if you have neither:
   `gh attestation verify steadycheck-<version>-windows-x86_64.exe -R kimjione1206/steadycheck --source-ref refs/tags/v<version> --signer-workflow kimjione1206/steadycheck/.github/workflows/release.yml`
4. Windows SmartScreen may warn that the publisher is unknown: the file is not signed with a code-signing certificate. The checks above are how you confirm it is the genuine build. With Smart App Control on, the exe is blocked outright — see [Before you start](#before-you-start).
5. Get [`tools/collect-info.ps1`](../tools/collect-info.ps1) (the "Download raw file" button) and put it in the same folder as the exe.

## 4. Run

Open PowerShell in that folder.
1. Close other programs (browser, games, launchers). The `share` test needs an otherwise idle PC.
2. Collect the hardware details and copy the printed text:
   `powershell -ExecutionPolicy Bypass -File .\collect-info.ps1`
   It prints Windows, CPU, motherboard, BIOS and memory module details — no serial numbers, user or computer name, network details or product keys. To read the steadycheck version it runs the exe for one second.
3. Run the test and save the JSON. For a memory overclock — and for a stable PC or a faulty memory module — use this command first (about 10 minutes):
   `.\steadycheck-<version>-windows-x86_64.exe mem --mb auto --seconds 600 --require-complete --keep-going 20 > result.json`
   `--require-complete` makes a run that was too short end as INCOMPLETE instead of PASS. `--keep-going 20` keeps the memory test going after the first error and stops after 20 — how many errors appear and how far apart helps show how close the setting is to its limit.
   To check the CPU too (a CPU overclock or undervolt, a faulty CPU), use `all` (about 15 minutes):
   `.\steadycheck-<version>-windows-x86_64.exe all --mb auto --seconds 300 > result.json`
   `all` runs cpu → share → mem, 300 seconds each, and stops at the first failure. Why `mem` comes first for memory: with `all` the memory test only starts after 10 minutes of CPU load, on a PC that is already hot, and is skipped altogether if the CPU part fails — so the time to the first memory error cannot be compared with other reports. With `mem` every report measures the same thing.
4. Check the result: right after the run, type `$LASTEXITCODE` and press Enter. 0 means PASS, 1 FAIL, 2 an unsupported environment, 3 a usage error, 4 = INCOMPLETE — no error, but the test did not get far enough; run again with more `--seconds`. INCOMPLETE appears only with `--require-complete`: if the memory base set did not finish or `mem.rounds_d` is below 4, a run without errors exits with 4 instead of 0. Use it whenever a short run must not be mistaken for a pass (for example an end-of-line check before shipping). When the test ends, the last line in the window is `판정: PASS` or `판정: FAIL` (or `판정: INCOMPLETE`, only with `--require-complete`; `판정` = verdict, in Korean). That line goes to stderr, so `> result.json` leaves it in the window; the full result is in `result.json`.

## 5. Read the result

Three values to read first (open `result.json` in Notepad):
1. `verdict`: `PASS`, `FAIL`, or `INCOMPLETE` (only with `--require-complete`).
2. `mem.error.at_ms`: on a memory FAIL, when the first error was caught — milliseconds from the start of the memory test (divide by 60000 for minutes). This is the number to compare with how long another tester took.
3. `mem.rounds_d`: should be at least 4. As a rule, run for roughly 3–4 × `mem.base_seconds_estimate` or more. If it is below 4, rerun with a larger `--seconds`.

Other fields:
- `warnings`: `mem_base_incomplete` means the memory base set did not finish in time, so the memory coverage table does not apply to this run; run again with more `--seconds` (`mem.base_seconds_estimate` shows how long the base set takes). The verdict is unchanged.
- `share.min_thread_messages`: 0 means a worker got no message; on a busy PC that can be other programs taking the CPU, so close them and run again.
- On a FAIL, the failing part has an `error` object. `mem.error.kind` is a diagnostic hint only: `read` (the value in memory is correct, the read path failed) or `stored` (the wrong value is in memory). `panic` is a steadycheck bug, not a hardware fault — please report it as a blank issue.
- `mem.errors_total`: the number of memory errors caught. With `--keep-going` 2 or more, `mem.errors` lists them (first 32; `--keep-going 1` behaves like the default); `mem.errors[].at_ms` is when each was caught (milliseconds from the start), so you can see how often errors occur.
- `mem.tested_percent`: the percentage of the computer's total physical memory (`mem.total_phys_bytes`) that was tested. The part used by Windows and other programs cannot be tested from inside Windows, so it is normally below 100.
- `whea` (informational, does not change the verdict): Windows hardware error log entries (WHEA-Logger) during the run (`whea.during_run`, by event ID) and in the 7 days before (`whea.before_7_days`). If there is a number under `during_run`, open Event Viewer → Windows Logs → System and look at the original WHEA-Logger entries.

## 6. Send the result

Open a [new issue](https://github.com/kimjione1206/steadycheck/issues/new/choose) and choose:
- **Tested on a known-unstable setting** — you ran steadycheck on a setting you know is unstable (overclock, tight timings, low voltage). PASS and FAIL both help; if the PC froze or rebooted and there is no `result.json`, report that too.
- **Result on a PC I believe is stable** — a PC you have good reason to think is stable, at default settings or a long-proven setting. PASS is wanted as much as FAIL: it is how we learn how often steadycheck raises a false alarm.
- **Tested a part known to be faulty** — a memory module or CPU that another tester has shown to be faulty, or whose replacement fixed the problem.

Paste the table printed by `collect-info.ps1`, the command you ran and the contents of `result.json`. To copy the result, open it in Notepad (`notepad result.json`) and copy everything; Windows PowerShell 5.1 saves it as UTF-16, which is fine, so there is no need to change the encoding. Do not paste red error text from the window: it can show your folder path, which contains your user name. Do not add serial numbers, names, IP addresses or product keys; the result JSON contains no personal data.

### No GitHub account?

Reply where you found steadycheck (for example the forum post) with these lines filled in:

```
steadycheck version: 0.6.0
Command: mem --mb auto --seconds 600 --require-complete --keep-going 20
Memory type: DDR4 / DDR5 / other
CPU / motherboard: (or paste the collect-info.ps1 table)
Changed: speed 6400 MT/s, timings 32-39-39-102, VDD 1.35 / VDDQ 1.35 / SoC 1.20 — or "stock" / "known-faulty part: memory module"
Other tester: name, first error after __ minutes (or "none")
steadycheck: verdict ___, mem.error.at_ms ___, mem.rounds_d ___
Froze / rebooted / blue screen: no (or which, and at which minute)
Result JSON: (paste the contents of result.json below)
```
