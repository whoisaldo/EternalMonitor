//! Start-on-boot registry control and the LAN-IP fallback — the only Win32
//! surface the GUI needs. Non-Windows builds get honest stubs so the rest of
//! the GUI stays portable.

#[cfg(windows)]
pub use windows_impl::{first_lan_ipv4, read_startup_registry, set_startup_registry};

/// Autostart is a Windows-only concept (HKCU Run key); the GUI hides the
/// checkbox off-Windows and these stubs keep call sites compiling.
#[cfg(not(windows))]
pub fn read_startup_registry() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_startup_registry(_enabled: bool) -> Result<(), String> {
    Err("Start on boot is only supported on Windows".to_string())
}

/// The route-probe in `detect_local_ip` covers non-Windows development
/// machines; there is no enumeration fallback there.
#[cfg(not(windows))]
pub fn first_lan_ipv4() -> Option<std::net::Ipv4Addr> {
    None
}

#[cfg(windows)]
mod windows_impl {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use tracing::info;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPEN_CREATE_OPTIONS,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    const RUN_KEY_PATH: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    const RUN_VALUE_NAME: PCWSTR = w!("EternalMonitor");

    /// Return the first non-loopback IPv4 address bound to a local adapter, via the Win32
    /// IP Helper API. Used only as a fallback when the route-probe trick can't determine the LAN IP.
    pub fn first_lan_ipv4() -> Option<std::net::Ipv4Addr> {
        use windows::Win32::NetworkManagement::IpHelper::{GetIpAddrTable, MIB_IPADDRTABLE};

        unsafe {
            // First call sizes the buffer.
            let mut size: u32 = 0;
            let _ = GetIpAddrTable(None, &mut size, false);
            if size == 0 {
                return None;
            }
            let mut buffer = vec![0u8; size as usize];
            let table = buffer.as_mut_ptr() as *mut MIB_IPADDRTABLE;
            if GetIpAddrTable(Some(table), &mut size, false) != 0 {
                return None;
            }

            let table = &*table;
            let rows =
                std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
            for row in rows {
                let ip = ipv4_from_inaddr(row.dwAddr);
                if !ip.is_loopback() && !ip.is_unspecified() {
                    return Some(ip);
                }
            }
        }
        None
    }

    /// Convert a Win32 `MIB_IPADDRROW.dwAddr` (an IPv4 address stored in **network byte order in
    /// memory**) into an `Ipv4Addr`. The four octets sit in memory as `[a, b, c, d]`, and
    /// `to_ne_bytes()` returns exactly those in-memory bytes on any platform, so this yields `a.b.c.d`.
    /// Do NOT switch to `to_be_bytes()`: on little-endian that reorders the octets to `d.c.b.a`
    /// (e.g. 192.168.1.1 -> 1.1.168.192). See the test below.
    fn ipv4_from_inaddr(dword: u32) -> std::net::Ipv4Addr {
        std::net::Ipv4Addr::from(dword.to_ne_bytes())
    }

    pub fn read_startup_registry() -> bool {
        match open_run_key(KEY_READ) {
            Ok(key) => {
                let result = unsafe {
                    RegQueryValueExW(key.0, RUN_VALUE_NAME, None, None, None, Some(&mut 0u32))
                };
                result == ERROR_SUCCESS
            }
            Err(_) => false,
        }
    }

    pub fn set_startup_registry(enabled: bool) -> Result<(), String> {
        if enabled {
            let exe_path = std::env::current_exe().map_err(|error| error.to_string())?;
            let key = create_run_key()?;
            let exe_path = utf16_bytes(exe_path.as_os_str());
            let status =
                unsafe { RegSetValueExW(key.0, RUN_VALUE_NAME, 0, REG_SZ, Some(&exe_path)) };
            if status == ERROR_SUCCESS {
                info!("Startup registry updated");
                Ok(())
            } else {
                Err(format!("Failed to set startup entry: {:?}", status))
            }
        } else {
            let key = open_run_key(KEY_SET_VALUE)?;
            let status = unsafe { RegDeleteValueW(key.0, RUN_VALUE_NAME) };
            if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
                info!("Startup registry updated");
                Ok(())
            } else {
                Err(format!("Failed to remove startup entry: {:?}", status))
            }
        }
    }

    fn create_run_key() -> Result<OwnedRegKey, String> {
        let mut key = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY_PATH,
                0,
                PCWSTR::null(),
                REG_OPEN_CREATE_OPTIONS(REG_OPTION_NON_VOLATILE.0),
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if status == ERROR_SUCCESS {
            Ok(OwnedRegKey(key))
        } else {
            Err(format!("Failed to open Run key: {:?}", status))
        }
    }

    fn open_run_key(
        access: windows::Win32::System::Registry::REG_SAM_FLAGS,
    ) -> Result<OwnedRegKey, String> {
        let mut key = HKEY::default();
        let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY_PATH, 0, access, &mut key) };
        if status == ERROR_SUCCESS {
            Ok(OwnedRegKey(key))
        } else {
            Err(format!("Failed to open Run key: {:?}", status))
        }
    }

    fn utf16_bytes(value: &OsStr) -> Vec<u8> {
        let wide: Vec<u16> = value.encode_wide().chain(std::iter::once(0)).collect();
        let len = wide.len() * std::mem::size_of::<u16>();
        let ptr = wide.as_ptr() as *const u8;
        unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
    }

    struct OwnedRegKey(HKEY);

    impl Drop for OwnedRegKey {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::ipv4_from_inaddr;
        use std::net::Ipv4Addr;

        #[test]
        fn ipv4_from_inaddr_preserves_network_order_octets() {
            // Windows stores 192.168.1.1 as the in-memory bytes [192, 168, 1, 1] (network order);
            // the u32 we read from that memory is from_ne_bytes([192,168,1,1]). Converting back
            // must give 192.168.1.1 — not the byte-reversed 1.1.168.192 that to_be_bytes() would
            // produce on LE.
            let dword = u32::from_ne_bytes([192, 168, 1, 1]);
            assert_eq!(ipv4_from_inaddr(dword), Ipv4Addr::new(192, 168, 1, 1));
        }
    }
}
