//! Launch-at-login (HKCU `...\Run` key) and the per-user single-instance
//! guard (a named mutex). Both are Windows-only; non-Windows targets get
//! inert fallbacks so the rest of the app can call these functions
//! unconditionally without `cfg` noise at every call site.
//!
//! The registry is the runtime source of truth for whether autostart is on:
//! `Config::start_with_windows` only mirrors the user's last intent for
//! serialization completeness (see `src/app.rs`'s `WindowOpened` handler,
//! which re-reads `autostart_enabled()` fresh every time the settings window
//! opens).

use std::path::Path;

/// Quote a path for a Run-key command line: wraps it in double quotes so a
/// path containing spaces still parses as a single token when Windows splits
/// the Run value into a command line. No arguments are appended.
fn quoted_command(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

#[cfg(windows)]
mod win {
    use super::quoted_command;
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegSetValueExW,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;

    /// Session-local (not `Global\`) since this is a per-user tray app, not a
    /// service shared across sessions.
    const MUTEX_NAME: &str = "ragequiet-single-instance";
    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const VALUE_NAME: &str = "ragequiet";

    /// Encode a Rust string as a null-terminated UTF-16 buffer suitable for
    /// passing as a `PCWSTR` (`*const u16`) to a Win32 call. The returned
    /// `Vec` must outlive any call using its pointer — callers keep it bound
    /// to a local rather than passing a temporary's pointer directly.
    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Take the per-user single-instance mutex. Returns `false` when another
    /// instance already holds it (`GetLastError() == ERROR_ALREADY_EXISTS`
    /// right after a successful create). The handle is intentionally leaked
    /// — it must live for the whole process, so it is never closed.
    pub fn acquire_single_instance() -> bool {
        let name = to_wide(MUTEX_NAME);
        // SAFETY: `name` is a valid null-terminated UTF-16 buffer alive for
        // the duration of this call; no security attributes are needed
        // (default, non-inheritable) and we don't request initial ownership.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            // Creation failed for some unexpected reason (e.g. resource
            // exhaustion): never block startup on a weird failure.
            return true;
        }
        // SAFETY: no pointer arguments; reads the calling thread's
        // last-error code, which `CreateMutexW` just set.
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        !already_exists
    }

    /// Open (creating if needed) `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
    /// with `KEY_SET_VALUE` access.
    fn open_run_key() -> anyhow::Result<HKEY> {
        let subkey = to_wide(RUN_KEY);
        let mut hkey: HKEY = std::ptr::null_mut();
        // SAFETY: `subkey` is a valid null-terminated wide string alive for
        // the call; `hkey` is a valid out-pointer we own on the stack; no
        // class name or security attributes are needed for a HKCU subkey,
        // and the disposition (created vs. opened) is not used.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            anyhow::bail!("RegCreateKeyExW failed (error {status})");
        }
        Ok(hkey)
    }

    /// Enable/disable launch-at-login. Enabling writes the quoted current
    /// exe path as a `REG_SZ`; disabling deletes the value (a value that is
    /// already missing is not an error).
    pub fn set_autostart(enabled: bool) -> anyhow::Result<()> {
        let hkey = open_run_key()?;
        let value_name = to_wide(VALUE_NAME);

        let result = if enabled {
            let exe = std::env::current_exe()?;
            let command = to_wide(&quoted_command(&exe));
            // REG_SZ data is the wide string's raw bytes including its
            // trailing NUL; `command` (u16, len N incl. the NUL) reinterpreted
            // as a byte slice of length N * 2.
            let data: &[u8] = unsafe {
                std::slice::from_raw_parts(command.as_ptr().cast::<u8>(), command.len() * 2)
            };
            // SAFETY: `hkey` came from the successful `open_run_key` call
            // above and is still open; `value_name`/`data` are valid slices
            // alive for the duration of this call.
            let status = unsafe {
                RegSetValueExW(hkey, value_name.as_ptr(), 0, REG_SZ, data.as_ptr(), data.len() as u32)
            };
            if status == 0 {
                Ok(())
            } else {
                Err(anyhow::anyhow!("RegSetValueExW failed (error {status})"))
            }
        } else {
            // SAFETY: `hkey` is the same valid open handle as above;
            // `value_name` is a valid wide string for the call.
            let status = unsafe { RegDeleteValueW(hkey, value_name.as_ptr()) };
            if status == 0 || status == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                Err(anyhow::anyhow!("RegDeleteValueW failed (error {status})"))
            }
        };

        // SAFETY: `hkey` is a valid open key handle from `open_run_key`.
        unsafe { RegCloseKey(hkey) };
        result
    }

    /// Whether the `ragequiet` value currently exists under the Run key.
    /// Any error (key/value missing, unexpected type, etc.) reads as `false`
    /// rather than propagating — this only drives a checkbox's initial
    /// state.
    pub fn autostart_enabled() -> bool {
        let subkey = to_wide(RUN_KEY);
        let value_name = to_wide(VALUE_NAME);
        let mut data = [0u8; 1024];
        let mut size = data.len() as u32;
        // SAFETY: `subkey`/`value_name` are valid null-terminated wide
        // strings for the call; `data`/`size` describe a real stack buffer
        // this function owns for the duration of the call.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                value_name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                data.as_mut_ptr().cast(),
                &mut size,
            )
        };
        status == 0
    }
}

#[cfg(windows)]
pub use win::{acquire_single_instance, autostart_enabled, set_autostart};

#[cfg(not(windows))]
pub fn acquire_single_instance() -> bool {
    true
}

#[cfg(not(windows))]
pub fn set_autostart(_enabled: bool) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn autostart_enabled() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn quotes_a_plain_path() {
        assert_eq!(quoted_command(Path::new(r"C:\Program Files\ragequiet\ragequiet.exe")), "\"C:\\Program Files\\ragequiet\\ragequiet.exe\"");
    }

    #[test]
    fn quotes_a_path_with_spaces_as_one_token() {
        let quoted = quoted_command(Path::new(r"C:\Users\a b\ragequiet.exe"));
        assert!(quoted.starts_with('"') && quoted.ends_with('"'));
        assert_eq!(quoted.matches('"').count(), 2, "must be exactly one quoted token: {quoted}");
    }

    #[test]
    fn quotes_a_simple_no_space_path() {
        assert_eq!(quoted_command(Path::new(r"C:\ragequiet.exe")), "\"C:\\ragequiet.exe\"");
    }

    /// Side-effecting registry round trip: writes and deletes the real
    /// `ragequiet` Run value. Ignored by default so normal `cargo test`/CI
    /// runs never touch the registry; run explicitly with
    /// `cargo test -- --ignored registry`. Leaves the Run value deleted
    /// afterward either way.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn registry_round_trip_set_then_read_then_clear() {
        set_autostart(true).expect("set_autostart(true) should succeed");
        assert!(autostart_enabled(), "value should exist right after enabling");

        set_autostart(false).expect("set_autostart(false) should succeed");
        assert!(!autostart_enabled(), "value should be gone right after disabling");
    }
}
