# Testing steadycheck on real hardware

[한국어](TESTING.ko.md)

## 1. What we are asking

Two questions need real PCs:
- Does steadycheck **catch** real instability — an unstable overclock, too-tight memory timings, too-low voltage, a faulty part?
- Does it stay **quiet** (PASS) on a PC that is stable?

What has been checked so far, all on GitHub's servers:
- **Simulated faults:** the memory base set runs on a simulated memory holding one fault at a time from a fixed catalogue (see the table in the [README](../README.md)).
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

Put the downloaded files in one folder. To open PowerShell there, right-click an empty space in that folder in File Explorer and choose "Open in Terminal"; type the commands on this page in that window.

In the commands, replace `<version>` with the number in the name of the file you downloaded. For example, for `steadycheck-0.5.0-windows-x86_64.exe` type `.\steadycheck-0.5.0-windows-x86_64.exe all --mb auto --seconds 300 > result.json` (and `refs/tags/v0.5.0` in step 3).

1. From the [Releases](https://github.com/kimjione1206/steadycheck/releases) page, get `steadycheck-<version>-windows-x86_64.exe` and `SHA256SUMS.txt`.
2. Check the fingerprint: `certutil -hashfile steadycheck-<version>-windows-x86_64.exe SHA256` must print the same value as `SHA256SUMS.txt`.
3. Optional, check where it was built (GitHub CLI; sign in once with `gh auth login` first):
   `gh attestation verify steadycheck-<version>-windows-x86_64.exe -R kimjione1206/steadycheck --source-ref refs/tags/v<version> --signer-workflow kimjione1206/steadycheck/.github/workflows/release.yml`
4. Windows SmartScreen may warn that the publisher is unknown: the file is not signed with a code-signing certificate. The checks above are how you confirm it is the genuine build. On Windows 11 with Smart App Control turned on, an unsigned exe is blocked outright with no "Run anyway" option, so steadycheck cannot run on that PC unless Smart App Control is off; that setting is your choice.
5. Get [`tools/collect-info.ps1`](../tools/collect-info.ps1) (the "Download raw file" button) and put it in the same folder as the exe.

## 4. Run

Open PowerShell in that folder.
1. Close other programs (browser, games, launchers). The `share` test needs an otherwise idle PC.
2. Collect the hardware details and copy the printed text:
   `powershell -ExecutionPolicy Bypass -File .\collect-info.ps1`
   It prints Windows, CPU, motherboard, BIOS and memory module details — no serial numbers, user or computer name, network details or product keys. To read the steadycheck version it runs the exe for one second.
3. Run the test and save the JSON:
   `.\steadycheck-<version>-windows-x86_64.exe all --mb auto --seconds 300 > result.json`
   `all` runs cpu → share → mem, 300 seconds each, so about 15 minutes; it stops at the first failure. For a memory-focused run use `mem --mb auto --seconds 600` instead.
4. Check the result: right after the run, type `$LASTEXITCODE` and press Enter. 0 means PASS, 1 FAIL, 2 an unsupported environment, 3 a usage error. When the test ends, the last line in the window is `판정: PASS` or `판정: FAIL` (`판정` = verdict, in Korean). That line goes to stderr, so `> result.json` leaves it in the window; the full result is in `result.json`.

## 5. Read the result

- `verdict`: `PASS` or `FAIL`.
- `warnings`: `mem_base_incomplete` means the memory base set did not finish in time, so the memory coverage table does not apply to this run; run again with more `--seconds` (`mem.base_seconds_estimate` shows how long the base set takes). The verdict is unchanged.
- `mem.rounds_d`: should be at least 4. As a rule, run for roughly 3–4 × `mem.base_seconds_estimate` or more. If `rounds_d` in the result is below 4, rerun with a larger `--seconds`.
- `share.min_thread_messages`: 0 means a worker got no message; on a busy PC that can be other programs taking the CPU, so close them and run again.
- On a FAIL, the failing part has an `error` object. `mem.error.kind` is a diagnostic hint only: `read` (the value in memory is correct, the read path failed) or `stored` (the wrong value is in memory). `panic` is a steadycheck bug, not a hardware fault — please report it as a blank issue.

## 6. Send the result

Open a [new issue](https://github.com/kimjione1206/steadycheck/issues/new/choose) and choose:
- **Tested on a known-unstable setting** — you ran steadycheck on a setting you know is unstable. PASS and FAIL both help.
- **FAIL on a PC I believe is stable** — steadycheck failed on a PC you have good reason to think is stable.

Paste the table printed by `collect-info.ps1`, the command you ran and the contents of `result.json`. To copy the result, open it in Notepad (`notepad result.json`) and copy everything; Windows PowerShell 5.1 saves it as UTF-16, which is fine, so there is no need to change the encoding. Do not paste red error text from the window: it can show your folder path, which contains your user name. Do not add serial numbers, names, IP addresses or product keys; the result JSON contains no personal data.
