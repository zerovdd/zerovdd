//! Windows Driver Store (`oemXX.inf` / `DIRID 13`) staging, inspection, and removal helpers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::InstallerError;

/// Driver INF filename distributed by `zerovdd`.
pub const DRIVER_INF_FILENAME: &str = "zerovdd.inf";

/// Driver UMDF binary filename copied into Driver Store (`DIRID 13`).
pub const DRIVER_DLL_FILENAME: &str = "zerovdd.dll";

/// Driver catalog filename referenced by `zerovdd.inf`.
pub const DRIVER_CAT_FILENAME: &str = "zerovdd.cat";

/// Driver Store package status and `DIRID 13` isolation metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DriverPackageStatus {
    /// Whether a matching `zerovdd` OEM INF is currently staged in the Windows Driver Store.
    pub installed: bool,
    /// Published OEM INF filename (for example `oem42.inf`).
    pub oem_inf_name: Option<String>,
    /// Full path to the published OEM INF in `%SystemRoot%\INF`.
    pub oem_inf_path: Option<PathBuf>,
    /// Full path to the staged INF inside `DriverStore\FileRepository\...` (`DIRID 13`).
    pub driver_store_path: Option<PathBuf>,
    /// `true` when staged inside `DriverStore\FileRepository` with `zerovdd.dll` alongside it.
    pub dirid_13_isolated: bool,
    /// Driver version parsed from `DriverVer` (for example `0.1.0.0`).
    pub driver_version: Option<String>,
    /// Driver date parsed from `DriverVer` (for example `11/24/2026`).
    pub driver_date: Option<String>,
    /// INF `Provider` string.
    pub provider_name: Option<String>,
    /// Authenticode signer subject name extracted from the staged catalog or driver DLL.
    pub signer_name: Option<String>,
}

/// Result of staging/installing a driver INF via `DiInstallDriverW`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DriverStageResult {
    /// Published OEM INF name assigned by Windows, if discovered.
    pub oem_inf_name: Option<String>,
    /// Staged Driver Store INF path, if discovered.
    pub driver_store_path: Option<PathBuf>,
    /// `true` if Windows indicated a reboot is required (`DI_NEEDREBOOT` or `ERROR_SUCCESS_REBOOT_REQUIRED`).
    pub reboot_required: bool,
}

/// Parses an INF `DriverVer = MM/DD/YYYY,X.Y.Z.W` value into `(date, version)`.
pub fn parse_driver_ver_value(value: &str) -> (Option<String>, Option<String>) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    let mut parts = trimmed.splitn(2, ',');
    let date = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let version = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    (date, version)
}

/// Checks whether `inf_text` belongs to the `zerovdd` virtual display INF package.
pub fn is_zerovdd_inf_content(inf_text: &str) -> bool {
    let lower = inf_text.to_ascii_lowercase();
    lower.contains(r"root\zerovdd") && lower.contains("zerovdd.dll")
}

/// Returns `true` when `driver_store_inf` resides under `DriverStore\FileRepository` and `dll_alongside` is true.
pub fn is_driver_store_isolated_path(driver_store_inf: &Path, dll_alongside: bool) -> bool {
    let normalized = driver_store_inf
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    dll_alongside && normalized.contains(r"\driverstore\filerepository\")
}

/// Decodes raw INF file bytes supporting UTF-16LE BOM, UTF-16BE BOM, UTF-8 BOM, and UTF-8/ASCII.
pub fn decode_inf_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let (pairs, _) = rest.as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|p| u16::from_le_bytes(*p)).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let (pairs, _) = rest.as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|p| u16::from_be_bytes(*p)).collect();
        return String::from_utf16_lossy(&units);
    }
    let utf8_bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(utf8_bytes).into_owned()
}

/// Parses the numeric index from a Windows published OEM INF filename (for example `oem42.inf` -> `Some(42)`).
pub fn parse_oem_inf_number(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_prefix("oem")?.strip_suffix(".inf")?;
    if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse::<u32>().ok()
}

/// Sorts `(oem_inf_name, value)` pairs numerically by OEM index (`oem9.inf` before `oem10.inf`).
pub fn sort_oem_inf_entries<T>(entries: &mut [(String, T)]) {
    entries.sort_by(|a, b| {
        let key_a = (
            parse_oem_inf_number(&a.0).unwrap_or(u32::MAX),
            a.0.to_ascii_lowercase(),
        );
        let key_b = (
            parse_oem_inf_number(&b.0).unwrap_or(u32::MAX),
            b.0.to_ascii_lowercase(),
        );
        key_a.cmp(&key_b)
    });
}

/// Validates that `inf_path` exists and points to a `.inf` file before invoking SetupAPI.
pub fn validate_inf_path(inf_path: &Path) -> Result<PathBuf, InstallerError> {
    if !inf_path.is_file() {
        return Err(InstallerError::InfNotFound(inf_path.to_path_buf()));
    }
    let is_inf = inf_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("inf"));
    if !is_inf {
        return Err(InstallerError::InvalidInput(format!(
            "expected a .inf driver package file, got {}",
            inf_path.display()
        )));
    }
    Ok(std::fs::canonicalize(inf_path).unwrap_or_else(|_| inf_path.to_path_buf()))
}

const HRESULT_ERROR_NO_MORE_ITEMS: u32 = 0x8007_0103;
const HRESULT_ERROR_REBOOT_REQUIRED: u32 = 0x8007_0BC2;

/// Classifies a Win32 `HRESULT` returned by `DiInstallDriverW`:
/// `Ok(true)` if a reboot is required (`ERROR_SUCCESS_REBOOT_REQUIRED`),
/// `Ok(false)` if the call was an idempotent no-op (`ERROR_NO_MORE_ITEMS`),
/// or `Err(code)` for any other error.
pub fn classify_di_install_hresult(code: u32) -> Result<bool, u32> {
    match code {
        HRESULT_ERROR_REBOOT_REQUIRED => Ok(true),
        // WHY: `DiInstallDriverW` returns `ERROR_NO_MORE_ITEMS` when the driver package is already
        // staged and all matching devices already use an equal or better driver version.
        HRESULT_ERROR_NO_MORE_ITEMS => Ok(false),
        other => Err(other),
    }
}

#[cfg(windows)]
mod win32 {
    use core::ffi::c_void;
    use std::fs;
    use std::path::{Path, PathBuf};

    use windows::core::{BOOL, HRESULT, PCWSTR};
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        DiInstallDriverW, SetupCloseInfFile, SetupFindFirstLineW, SetupGetInfDriverStoreLocationW,
        SetupGetStringFieldW, SetupOpenInfFileW, SetupUninstallOEMInfW, DIINSTALLDRIVER_FLAGS,
        DIIRFLAG_FORCE_INF, INFCONTEXT, INF_STYLE_WIN4, SUOI_FORCEDELETE,
    };
    use windows::Win32::Foundation::{
        GetLastError, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS_REBOOT_REQUIRED,
    };
    use windows::Win32::Security::Cryptography::{
        CertCloseStore, CertFreeCertificateContext, CertGetNameStringW, CryptMsgClose,
        CryptMsgGetParam, CryptQueryObject, CERT_CONTEXT, CERT_NAME_SIMPLE_DISPLAY_TYPE,
        CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED, CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
        CERT_QUERY_CONTENT_TYPE_FLAGS, CERT_QUERY_FORMAT_FLAG_BINARY, CERT_QUERY_OBJECT_FILE,
        CMSG_SIGNER_INFO, CMSG_SIGNER_INFO_PARAM, HCERTSTORE,
    };

    use super::{
        classify_di_install_hresult, decode_inf_bytes, is_driver_store_isolated_path,
        is_zerovdd_inf_content, parse_driver_ver_value, sort_oem_inf_entries, validate_inf_path,
        DriverPackageStatus, DriverStageResult, DRIVER_CAT_FILENAME, DRIVER_DLL_FILENAME,
    };
    use crate::InstallerError;

    struct OwnedInf(isize);

    impl Drop for OwnedInf {
        fn drop(&mut self) {
            if self.0 != -1 && self.0 != 0 {
                // SAFETY: `self.0` was opened by `SetupOpenInfFileW` and is closed once on drop.
                unsafe {
                    SetupCloseInfFile(self.0 as *const c_void);
                }
            }
        }
    }

    fn utf16_nul_to_string(buf: &[u16]) -> String {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    fn windows_inf_dir() -> PathBuf {
        let sys_root = std::env::var_os("SystemRoot")
            .or_else(|| std::env::var_os("WINDIR"))
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        sys_root.join("INF")
    }

    fn query_inf_driver_store_location(inf_path: &Path) -> Option<PathBuf> {
        let wide_inf: Vec<u16> = inf_path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        let mut required: u32 = 0;
        // SAFETY: Querying required UTF-16 character count for Driver Store path.
        let first = unsafe {
            SetupGetInfDriverStoreLocationW(
                PCWSTR(wide_inf.as_ptr()),
                None,
                None,
                &mut [],
                Some(&mut required),
            )
        };
        if let Err(err) = first {
            if err.code() != HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0) {
                return None;
            }
        }
        if required == 0 {
            return None;
        }
        let mut buf = vec![0u16; required as usize];
        // SAFETY: `buf` has capacity `required` UTF-16 code units.
        let res = unsafe {
            SetupGetInfDriverStoreLocationW(
                PCWSTR(wide_inf.as_ptr()),
                None,
                None,
                &mut buf,
                Some(&mut required),
            )
        };
        if res.is_ok() {
            let text = utf16_nul_to_string(&buf);
            if !text.is_empty() {
                return Some(PathBuf::from(text));
            }
        }
        None
    }

    fn read_inf_key_value(hinf: *const c_void, section: &str, key: &str) -> Option<String> {
        let wide_sec: Vec<u16> = section.encode_utf16().chain(core::iter::once(0)).collect();
        let wide_key: Vec<u16> = key.encode_utf16().chain(core::iter::once(0)).collect();
        let mut ctx = INFCONTEXT::default();
        // SAFETY: `hinf` is an open INF handle and section/key are NUL-terminated UTF-16.
        let found = unsafe {
            SetupFindFirstLineW(
                hinf,
                PCWSTR(wide_sec.as_ptr()),
                PCWSTR(wide_key.as_ptr()),
                &mut ctx,
            )
        };
        if found.is_err() {
            return None;
        }

        let mut required: u32 = 0;
        // SAFETY: First call queries required string length for field index 0 (whole value after `=`).
        let _ = unsafe { SetupGetStringFieldW(&ctx, 1, None, Some(&mut required)) };
        if required == 0 {
            return None;
        }
        let mut buf = vec![0u16; required as usize];
        // SAFETY: `buf` holds `required` UTF-16 code units.
        let ok = unsafe { SetupGetStringFieldW(&ctx, 1, Some(&mut buf), Some(&mut required)) };
        if ok.is_ok() {
            let v = utf16_nul_to_string(&buf);
            if !v.is_empty() {
                return Some(v);
            }
        }
        None
    }

    fn read_inf_metadata(inf_path: &Path) -> (Option<String>, Option<String>, Option<String>) {
        let wide_inf: Vec<u16> = inf_path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        let mut error_line: u32 = 0;
        // SAFETY: `wide_inf` is NUL-terminated UTF-16 and `error_line` is a stack `u32`.
        let raw_hinf = unsafe {
            SetupOpenInfFileW(
                PCWSTR(wide_inf.as_ptr()),
                None,
                INF_STYLE_WIN4,
                Some(&mut error_line),
            )
        };
        if raw_hinf == (-1isize as *mut c_void) || raw_hinf.is_null() {
            return (None, None, None);
        }
        let inf = OwnedInf(raw_hinf as isize);
        let hinf = inf.0 as *const c_void;

        let provider = read_inf_key_value(hinf, "Version", "Provider");
        let driver_ver_raw = read_inf_key_value(hinf, "Version", "DriverVer");
        let (date, ver_first) = match driver_ver_raw {
            Some(ref val) => parse_driver_ver_value(val),
            None => (None, None),
        };
        let version = ver_first.or_else(|| {
            let wide_sec: Vec<u16> = "Version"
                .encode_utf16()
                .chain(core::iter::once(0))
                .collect();
            let wide_key: Vec<u16> = "DriverVer"
                .encode_utf16()
                .chain(core::iter::once(0))
                .collect();
            let mut ctx = INFCONTEXT::default();
            // SAFETY: `hinf` is an open INF handle.
            if unsafe {
                SetupFindFirstLineW(
                    hinf,
                    PCWSTR(wide_sec.as_ptr()),
                    PCWSTR(wide_key.as_ptr()),
                    &mut ctx,
                )
            }
            .is_ok()
            {
                let mut buf = [0u16; 128];
                let mut req: u32 = 0;
                // SAFETY: Field index 2 holds the version component after `,` in `DriverVer`.
                if unsafe { SetupGetStringFieldW(&ctx, 2, Some(&mut buf), Some(&mut req)) }.is_ok()
                {
                    let s = utf16_nul_to_string(&buf);
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
            None
        });
        (date, version, provider)
    }

    fn query_file_signer_name(path: &Path) -> Option<String> {
        if !path.is_file() {
            return None;
        }
        let wide_path: Vec<u16> = path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();

        let mut hstore = HCERTSTORE::default();
        let mut hmsg: *mut c_void = core::ptr::null_mut();

        // SAFETY: `wide_path` is NUL-terminated UTF-16 and output pointers are valid stack locals.
        let query_ok = unsafe {
            CryptQueryObject(
                CERT_QUERY_OBJECT_FILE,
                wide_path.as_ptr().cast::<c_void>(),
                CERT_QUERY_CONTENT_TYPE_FLAGS(
                    CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED.0
                        | CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED.0,
                ),
                CERT_QUERY_FORMAT_FLAG_BINARY,
                0,
                None,
                None,
                None,
                Some(&mut hstore),
                Some(&mut hmsg),
                None,
            )
        };
        if query_ok.is_err() || hstore.is_invalid() || hmsg.is_null() {
            if !hmsg.is_null() {
                // SAFETY: `hmsg` was returned by `CryptQueryObject` and is closed once.
                unsafe {
                    let _ = CryptMsgClose(Some(hmsg));
                }
            }
            if !hstore.is_invalid() {
                // SAFETY: `hstore` was returned by `CryptQueryObject` and is closed once.
                unsafe {
                    let _ = CertCloseStore(Some(hstore), 0);
                }
            }
            return None;
        }

        let mut signer_size: u32 = 0;
        // SAFETY: First call queries byte size of `CMSG_SIGNER_INFO`.
        let size_ok =
            unsafe { CryptMsgGetParam(hmsg, CMSG_SIGNER_INFO_PARAM, 0, None, &mut signer_size) };
        let mut result = None;
        if size_ok.is_ok() && (signer_size as usize) >= core::mem::size_of::<CMSG_SIGNER_INFO>() {
            let word_count = (signer_size as usize).div_ceil(core::mem::size_of::<u64>());
            let mut buf = vec![0u64; word_count];
            // SAFETY: `buf` is 8-byte aligned and provides at least `signer_size` bytes capacity.
            if unsafe {
                CryptMsgGetParam(
                    hmsg,
                    CMSG_SIGNER_INFO_PARAM,
                    0,
                    Some(buf.as_mut_ptr().cast::<c_void>()),
                    &mut signer_size,
                )
            }
            .is_ok()
                && (signer_size as usize) >= core::mem::size_of::<CMSG_SIGNER_INFO>()
            {
                // SAFETY: `buf` is `u64`-aligned and `signer_size >= size_of::<CMSG_SIGNER_INFO>()`.
                let signer_info = unsafe { &*(buf.as_ptr().cast::<CMSG_SIGNER_INFO>()) };
                let cert_info = windows::Win32::Security::Cryptography::CERT_INFO {
                    Issuer: signer_info.Issuer,
                    SerialNumber: signer_info.SerialNumber,
                    ..Default::default()
                };
                // SAFETY: Searches the PKCS#7 embedded certificate store for the matching signer cert.
                let cert_ctx: *const CERT_CONTEXT = unsafe {
                    windows::Win32::Security::Cryptography::CertFindCertificateInStore(
                        hstore,
                        windows::Win32::Security::Cryptography::X509_ASN_ENCODING
                            | windows::Win32::Security::Cryptography::PKCS_7_ASN_ENCODING,
                        0,
                        windows::Win32::Security::Cryptography::CERT_FIND_SUBJECT_CERT,
                        Some((&raw const cert_info).cast::<c_void>()),
                        None,
                    )
                };
                if !cert_ctx.is_null() {
                    let mut name_buf = [0u16; 256];
                    // SAFETY: `cert_ctx` is non-null and `name_buf` is stack-allocated UTF-16.
                    let chars = unsafe {
                        CertGetNameStringW(
                            cert_ctx,
                            CERT_NAME_SIMPLE_DISPLAY_TYPE,
                            0,
                            None,
                            Some(&mut name_buf),
                        )
                    };
                    if chars > 1 {
                        let s = utf16_nul_to_string(&name_buf);
                        if !s.is_empty() {
                            result = Some(s);
                        }
                    }
                    // SAFETY: `cert_ctx` was returned by `CertFindCertificateInStore` and is freed once.
                    unsafe {
                        let _ = CertFreeCertificateContext(Some(cert_ctx));
                    }
                }
            }
        }

        // SAFETY: `hmsg` and `hstore` were opened by `CryptQueryObject`.
        unsafe {
            let _ = CryptMsgClose(Some(hmsg));
            let _ = CertCloseStore(Some(hstore), 0);
        }
        result
    }

    fn find_matching_oem_infs() -> Vec<(String, PathBuf)> {
        let inf_dir = windows_inf_dir();
        let Ok(entries) = fs::read_dir(&inf_dir) else {
            return Vec::new();
        };
        let mut matches = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if !(lower.starts_with("oem") && lower.ends_with(".inf")) {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let text = decode_inf_bytes(&bytes);
            if is_zerovdd_inf_content(&text) {
                matches.push((name.to_string(), path));
            }
        }
        sort_oem_inf_entries(&mut matches);
        matches
    }

    /// Stages and installs `inf_path` into the Windows Driver Store using `DiInstallDriverW`.
    pub fn stage_and_install_driver_inf(
        inf_path: &Path,
        force_reinstall: bool,
    ) -> Result<DriverStageResult, InstallerError> {
        let canonical = validate_inf_path(inf_path)?;
        let wide_inf: Vec<u16> = canonical
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();

        let flags = if force_reinstall {
            DIIRFLAG_FORCE_INF
        } else {
            DIINSTALLDRIVER_FLAGS(0)
        };
        let mut need_reboot = BOOL(0);
        // SAFETY: `wide_inf` is NUL-terminated UTF-16 pointing to a validated `.inf` file.
        let res = unsafe {
            DiInstallDriverW(
                None,
                PCWSTR(wide_inf.as_ptr()),
                flags,
                Some(&mut need_reboot),
            )
        };
        // SAFETY: `GetLastError` retrieves thread-local reboot/error code immediately after `DiInstallDriverW`.
        let last_err = unsafe { GetLastError() };
        let mut reboot_required =
            need_reboot.as_bool() || last_err == ERROR_SUCCESS_REBOOT_REQUIRED;

        if let Err(err) = res {
            match classify_di_install_hresult(err.code().0 as u32) {
                Ok(needs_reboot) => reboot_required |= needs_reboot,
                Err(code) => {
                    return Err(InstallerError::Win32 {
                        operation: "DiInstallDriverW",
                        code,
                    });
                }
            }
        }

        let driver_store_path = query_inf_driver_store_location(&canonical);
        let status = query_driver_package_status().unwrap_or_default();
        Ok(DriverStageResult {
            oem_inf_name: status.oem_inf_name,
            driver_store_path: driver_store_path.or(status.driver_store_path),
            reboot_required,
        })
    }

    /// Removes all staged `zerovdd` OEM INF packages from the Windows Driver Store.
    pub fn uninstall_matching_oem_infs() -> Result<(Vec<String>, bool), InstallerError> {
        let matches = find_matching_oem_infs();
        let mut removed = Vec::new();
        let mut reboot_required = false;

        for (oem_name, _) in matches {
            let wide_name: Vec<u16> = oem_name.encode_utf16().chain(core::iter::once(0)).collect();
            // SAFETY: `wide_name` is a NUL-terminated published OEM INF filename (e.g. `oem42.inf`).
            let ok = unsafe {
                SetupUninstallOEMInfW(PCWSTR(wide_name.as_ptr()), SUOI_FORCEDELETE, None)
            };
            // SAFETY: `GetLastError` checks error/reboot status immediately after `SetupUninstallOEMInfW`.
            let last_err = unsafe { GetLastError() };
            if !ok.as_bool() {
                if last_err == ERROR_SUCCESS_REBOOT_REQUIRED {
                    reboot_required = true;
                } else {
                    return Err(InstallerError::Win32 {
                        operation: "SetupUninstallOEMInfW",
                        code: last_err.0,
                    });
                }
            } else if last_err == ERROR_SUCCESS_REBOOT_REQUIRED {
                reboot_required = true;
            }
            removed.push(oem_name);
        }

        Ok((removed, reboot_required))
    }

    /// Queries the published OEM INF, `DIRID 13` Driver Store path, version, and signer for `zerovdd`.
    pub fn query_driver_package_status() -> Result<DriverPackageStatus, InstallerError> {
        let matches = find_matching_oem_infs();
        let Some((oem_inf_name, oem_inf_path)) = matches.into_iter().last() else {
            return Ok(DriverPackageStatus::default());
        };

        let driver_store_path = query_inf_driver_store_location(&oem_inf_path);
        let dirid_13_isolated = driver_store_path.as_ref().is_some_and(|store_inf| {
            let dll_alongside = store_inf
                .parent()
                .is_some_and(|dir| dir.join(DRIVER_DLL_FILENAME).is_file());
            is_driver_store_isolated_path(store_inf, dll_alongside)
        });

        let (driver_date, driver_version, provider_name) = read_inf_metadata(&oem_inf_path);
        let signer_name = driver_store_path.as_ref().and_then(|store_inf| {
            let parent = store_inf.parent()?;
            query_file_signer_name(&parent.join(DRIVER_CAT_FILENAME))
                .or_else(|| query_file_signer_name(&parent.join(DRIVER_DLL_FILENAME)))
        });

        Ok(DriverPackageStatus {
            installed: true,
            oem_inf_name: Some(oem_inf_name),
            oem_inf_path: Some(oem_inf_path),
            driver_store_path,
            dirid_13_isolated,
            driver_version,
            driver_date,
            provider_name,
            signer_name,
        })
    }
}

#[cfg(windows)]
pub use win32::{
    query_driver_package_status, stage_and_install_driver_inf, uninstall_matching_oem_infs,
};
