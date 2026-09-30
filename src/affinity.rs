/// 현재 스레드를 논리 CPU 하나에 고정한다. Windows 가 아니면 아무것도 안 하고 false.
pub fn pin_current_thread(cpu: usize) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};
        if cpu >= usize::BITS as usize {
            return false;
        }
        unsafe { SetThreadAffinityMask(GetCurrentThread(), 1usize << cpu) != 0 }
    }
    #[cfg(not(windows))]
    {
        let _ = cpu;
        false
    }
}
