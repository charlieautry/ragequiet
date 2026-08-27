//! Idle-cheap CPU usage readout for the settings window (spec §7): this
//! process's own CPU time via `GetProcessTimes`, turned into a percent of one
//! core by comparing two `(cpu_100ns, wall_100ns)` samples. Windows-only;
//! non-Windows targets get an inert fallback so `app.rs` can call
//! `process_cpu_100ns` unconditionally.

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    fn filetime_to_u64(ft: FILETIME) -> u64 {
        (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
    }

    /// This process's total (kernel + user) CPU time in 100-ns units since
    /// process start, or `None` on the (practically never-hit) failure path.
    pub fn process_cpu_100ns() -> Option<u64> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: all four out-params are valid, owned `FILETIME`s on the
        // stack; `GetCurrentProcess` returns a pseudo-handle that needs no
        // cleanup.
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &raw mut creation,
                &raw mut exit,
                &raw mut kernel,
                &raw mut user,
            )
        };
        if ok == 0 {
            return None;
        }
        Some(filetime_to_u64(kernel) + filetime_to_u64(user))
    }
}

#[cfg(not(windows))]
mod win {
    pub fn process_cpu_100ns() -> Option<u64> {
        None
    }
}

pub use win::process_cpu_100ns;

/// CPU percent of one core between two `(cpu_100ns, wall_100ns)` samples
/// (both in 100-ns units, matching Windows `FILETIME` granularity). Guards a
/// zero or backwards wall-clock delta (first sample, or a clock hiccup) by
/// returning `0.0` rather than dividing by zero or reporting a negative
/// figure.
pub fn cpu_percent(prev: (u64, u64), cur: (u64, u64)) -> f32 {
    let wall_delta = cur.1.saturating_sub(prev.1);
    if wall_delta == 0 {
        return 0.0;
    }
    let cpu_delta = cur.0.saturating_sub(prev.0);
    (cpu_delta as f64 / wall_delta as f64 * 100.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifty_percent_of_one_core() {
        // 0.5 s of CPU time consumed over 1.0 s of wall time.
        let prev = (0u64, 0u64);
        let cur = (5_000_000u64, 10_000_000u64);
        assert!((cpu_percent(prev, cur) - 50.0).abs() < 0.01);
    }

    #[test]
    fn zero_percent_when_no_cpu_time_elapsed() {
        let prev = (1_000_000u64, 0u64);
        let cur = (1_000_000u64, 10_000_000u64);
        assert_eq!(cpu_percent(prev, cur), 0.0);
    }

    #[test]
    fn zero_or_backwards_wall_delta_is_guarded_to_zero() {
        let prev = (0u64, 5_000_000u64);
        assert_eq!(cpu_percent(prev, (1_000_000u64, 5_000_000u64)), 0.0, "zero wall delta");
        assert_eq!(cpu_percent(prev, (1_000_000u64, 4_000_000u64)), 0.0, "backwards wall delta");
    }
}
