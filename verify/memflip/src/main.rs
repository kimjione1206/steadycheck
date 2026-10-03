//! memflip: 실행 중인 다른 프로세스의 가장 큰 읽기·쓰기 개인 메모리 영역에서 비트 하나를 바깥에서 뒤집거나 고정한다.
//! 검사기 실행 파일을 고치지 않고 메모리 불량을 흉내 내기 위한 검증 도구.

#[cfg(not(windows))]
fn main() {
    eprintln!("윈도우 전용");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::run());
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::Debug::{GetThreadContext, ReadProcessMemory, WriteProcessMemory, CONTEXT, CONTEXT_CONTROL_AMD64};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32};
    use windows_sys::Win32::System::Memory::{VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_PRIVATE, PAGE_READWRITE};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenThread, ResumeThread, SuspendThread, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE, THREAD_GET_CONTEXT, THREAD_SUSPEND_RESUME,
    };

    const MIB: usize = 1 << 20;
    const USAGE: &str = "사용법: memflip --pid <PID> --mode once|stuck0|stuck1|watch --bit <0-63> [--word N] [--delay-ms ms] [--seconds s]";

    struct Args {
        pid: u32,
        mode: String,
        bit: u32,
        word: u64,
        delay_ms: u64,
        seconds: u64,
    }

    fn parse() -> Result<Args, String> {
        let mut a = Args { pid: 0, mode: String::new(), bit: 64, word: 0, delay_ms: 2000, seconds: 60 };
        let mut it = std::env::args().skip(1);
        while let Some(k) = it.next() {
            let v = it.next().ok_or(format!("{k} 값 없음"))?;
            let num = || v.parse::<u64>().map_err(|_| format!("{k} 숫자 아님: {v}"));
            match k.as_str() {
                "--pid" => a.pid = u32::try_from(num()?).map_err(|_| format!("--pid 범위 밖: {v}"))?,
                "--mode" => a.mode = v.clone(),
                "--bit" => a.bit = num()?.min(64) as u32,
                "--word" => a.word = num()?,
                "--delay-ms" => a.delay_ms = num()?,
                "--seconds" => a.seconds = num()?,
                _ => return Err(format!("모르는 인자: {k}")),
            }
        }
        if a.pid == 0 {
            return Err("--pid 필요".into());
        }
        if !matches!(a.mode.as_str(), "once" | "stuck0" | "stuck1" | "watch") {
            return Err(format!("--mode 가 once|stuck0|stuck1|watch 아님: {}", a.mode));
        }
        if a.bit > 63 {
            return Err("--bit 0~63 필요".into());
        }
        Ok(a)
    }

    pub fn run() -> i32 {
        let a = match parse() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("{e}\n{USAGE}");
                return 1;
            }
        };
        let h = unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_QUERY_INFORMATION, 0, a.pid) };
        if h.is_null() {
            eprintln!("프로세스 열기 실패: pid {}", a.pid);
            return 3;
        }
        let code = inject(h, &a);
        unsafe { CloseHandle(h) };
        code
    }

    /// 다른 프로세스의 addr 에서 8바이트 읽기 (프로세스가 끝났으면 None)
    fn read(h: HANDLE, addr: usize) -> Option<u64> {
        let (mut v, mut n) = (0u64, 0usize);
        let ok = unsafe { ReadProcessMemory(h, addr as *const c_void, &mut v as *mut u64 as *mut c_void, 8, &mut n) };
        (ok != 0 && n == 8).then_some(v)
    }

    /// 다른 프로세스의 addr 에 8바이트 쓰기
    fn write(h: HANDLE, addr: usize, v: u64) -> bool {
        let mut n = 0usize;
        let ok = unsafe { WriteProcessMemory(h, addr as *const c_void, &v as *const u64 as *const c_void, 8, &mut n) };
        ok != 0 && n == 8
    }

    /// GetThreadContext 가 요구하는 16바이트 정렬
    #[repr(C, align(16))]
    struct Ctx(CONTEXT);

    /// 대상 프로세스의 스레드를 모두 멈춘다 — 읽고 고쳐 쓰는 사이 검사기가 그 칸을 바꿔 묵은 값을 덮어쓰지 않게
    fn suspend_all(pid: u32) -> Vec<HANDLE> {
        let mut out = Vec::new();
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut te: THREADENTRY32 = unsafe { std::mem::zeroed() };
        te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut more = unsafe { Thread32First(snap, &mut te) } != 0;
        while more {
            if te.th32OwnerProcessID == pid {
                let t = unsafe { OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT, 0, te.th32ThreadID) };
                if !t.is_null() {
                    if unsafe { SuspendThread(t) } != u32::MAX {
                        // SuspendThread 는 비동기 — 문맥을 읽어 실제로 멈출 때까지 기다린다
                        let mut c: Ctx = unsafe { std::mem::zeroed() };
                        c.0.ContextFlags = CONTEXT_CONTROL_AMD64;
                        unsafe { GetThreadContext(t, &mut c.0) };
                        out.push(t);
                    } else {
                        unsafe { CloseHandle(t) };
                    }
                }
            }
            more = unsafe { Thread32Next(snap, &mut te) } != 0;
        }
        unsafe { CloseHandle(snap) };
        out
    }

    fn resume_all(threads: Vec<HANDLE>) {
        for t in threads {
            unsafe {
                ResumeThread(t);
                CloseHandle(t);
            }
        }
    }

    /// 검사기를 멈춘 채 읽어 f 로 고친 값을 쓴다 → (읽은 값, 쓴 값). 고칠 게 없으면 쓰지 않고 같은 값 둘. 실패면 None
    fn modify(h: HANDLE, pid: u32, addr: usize, f: impl Fn(u64) -> u64) -> Option<(u64, u64)> {
        let threads = suspend_all(pid);
        let r = read(h, addr).and_then(|v| {
            let w = f(v);
            (w == v || write(h, addr, w)).then_some((v, w))
        });
        resume_all(threads);
        r
    }

    /// 주소 0 부터 끝까지 훑어 확정·개인·읽기쓰기 영역 중 가장 큰 것 (시작, 크기)
    fn largest_region(h: HANDLE) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        let mut addr = 0usize;
        loop {
            let mut m: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            if unsafe { VirtualQueryEx(h, addr as *const c_void, &mut m, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
                break;
            }
            let (base, size) = (m.BaseAddress as usize, m.RegionSize);
            if m.State == MEM_COMMIT && m.Type == MEM_PRIVATE && m.Protect == PAGE_READWRITE && best.is_none_or(|(_, s)| size > s) {
                best = Some((base, size));
            }
            match base.checked_add(size) {
                Some(next) if next > addr => addr = next,
                _ => break,
            }
        }
        best
    }

    fn inject(h: HANDLE, a: &Args) -> i32 {
        // 검사기가 버퍼를 만들고 쓰기 시작할 때까지 기다린다
        std::thread::sleep(Duration::from_millis(a.delay_ms));
        let Some((base, size)) = largest_region(h).filter(|&(_, s)| s >= 4 * MIB) else {
            eprintln!("4MiB 이상 읽기·쓰기 개인 영역 없음");
            return 2;
        };
        // 영역 앞쪽 머리말과 끝 1MiB 씩을 피한다
        let words = ((size - 2 * MIB) / 8) as u64;
        let addr = base + MIB + (a.word % words) as usize * 8;
        let mask = 1u64 << a.bit;
        let Some(first) = read(h, addr) else {
            eprintln!("대상 주소 읽기 실패: {addr:#x}");
            return 3;
        };
        let (mut before, mut after, mut writes, mut t_first_ms) = (first, first, 0u64, None);
        let start = Instant::now();
        let limit = Duration::from_secs(a.seconds);
        match a.mode.as_str() {
            "once" => {
                let Some((v, w)) = modify(h, a.pid, addr, |v| v ^ mask) else {
                    eprintln!("대상 주소 쓰기 실패: {addr:#x}");
                    return 3;
                };
                (before, after) = (v, w);
                writes = 1;
                t_first_ms = Some(0);
            }
            "stuck0" | "stuck1" => {
                let fixed = if a.mode == "stuck1" { mask } else { 0 };
                // 시간이 다 되거나 프로세스가 끝날(읽기 실패) 때까지 그 비트를 고정값으로 되돌린다
                while start.elapsed() < limit {
                    let Some(v) = read(h, addr) else { break };
                    if v & mask != fixed {
                        // 멈춘 채 다시 읽어 고친다 (그새 검사기가 바꿨을 수 있다)
                        let Some((v, w)) = modify(h, a.pid, addr, |v| (v & !mask) | fixed) else { break };
                        if w != v {
                            if writes == 0 {
                                (before, after) = (v, w);
                                t_first_ms = Some(start.elapsed().as_millis() as u64);
                            }
                            writes += 1;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            _ => {
                // watch: 읽기만 하는 대조군
                while start.elapsed() < limit && read(h, addr).is_some() {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        let t = t_first_ms.map_or("null".to_string(), |t| t.to_string());
        println!(
            "{{\"mode\":\"{}\",\"region_base\":\"{base:#x}\",\"region_size\":{size},\"addr\":\"{addr:#x}\",\"bit\":{},\"before\":\"{before:#018x}\",\"after\":\"{after:#018x}\",\"writes\":{writes},\"t_first_ms\":{t}}}",
            a.mode, a.bit
        );
        0
    }
}
