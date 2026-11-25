//! Pure-Rust X.509 SHA-256 thumbprint verification, install-state tracking, and Windows cert store helpers.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use zerovdd_config::default_config_dir;

use crate::InstallerError;

/// Current JSON schema version for persisted [`InstallState`].
pub const INSTALL_STATE_SCHEMA_VERSION: u32 = 1;

/// Filename used to track installer-managed development certificates.
pub const INSTALL_STATE_FILENAME: &str = "install-state.json";

/// Target Windows LocalMachine certificate stores used for development test-signing certificates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertStoreLocation {
    /// `LocalMachine\Root` (Trusted Root Certification Authorities).
    LocalMachineRoot,
    /// `LocalMachine\TrustedPublisher` (Trusted Publishers).
    LocalMachineTrustedPublisher,
}

impl CertStoreLocation {
    /// Returns the Windows certificate store name string for `CertOpenStore`.
    pub const fn windows_store_name(self) -> &'static str {
        match self {
            Self::LocalMachineRoot => "Root",
            Self::LocalMachineTrustedPublisher => "TrustedPublisher",
        }
    }

    /// Returns the two stores required for test-signed Universal Display driver packages.
    pub const fn default_dev_stores() -> [Self; 2] {
        [Self::LocalMachineRoot, Self::LocalMachineTrustedPublisher]
    }
}

/// Verified X.509 DER certificate ready to be imported into LocalMachine certificate stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDevCert {
    /// Raw DER-encoded X.509 certificate bytes.
    pub der_bytes: Vec<u8>,
    /// Raw 32-byte SHA-256 digest of `der_bytes`.
    pub sha256_bytes: [u8; 32],
    /// Lowercase 64-character hexadecimal SHA-256 thumbprint.
    pub sha256_hex: String,
}

/// Record of a development certificate installed by `zerovdd-installer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledCertRecord {
    /// Lowercase 64-character hexadecimal SHA-256 thumbprint of the installed certificate.
    pub sha256: String,
    /// Certificate stores into which the installer placed this certificate.
    pub stores: Vec<CertStoreLocation>,
}

/// Persisted installer state tracking certificates added by `zerovdd-installer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallState {
    /// Schema version (must equal [`INSTALL_STATE_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Development certificates installed by `zerovdd-installer` for clean removal on uninstall.
    #[serde(default)]
    pub installed_dev_certs: Vec<InstalledCertRecord>,
}

impl Default for InstallState {
    fn default() -> Self {
        Self {
            schema_version: INSTALL_STATE_SCHEMA_VERSION,
            installed_dev_certs: Vec::new(),
        }
    }
}

/// Returns the default path `%ProgramData%\zerovdd\install-state.json` (or XDG equivalent).
pub fn default_install_state_path() -> PathBuf {
    default_config_dir().join(INSTALL_STATE_FILENAME)
}

/// Parses and normalizes a SHA-256 hexadecimal thumbprint string into 32 raw bytes.
///
/// Accepts optional `sha256:` prefix (case-insensitive) and ignores `:`, `-`, and ASCII whitespace.
pub fn parse_sha256_hex(input: &str) -> Result<[u8; 32], InstallerError> {
    let trimmed = input.trim();
    let without_prefix = if trimmed
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("sha256:"))
    {
        &trimmed[7..]
    } else {
        trimmed
    };

    let mut hex_digits = Vec::with_capacity(64);
    for b in without_prefix.bytes() {
        if matches!(b, b':' | b'-' | b' ' | b'\t') {
            continue;
        }
        if !b.is_ascii_hexdigit() {
            return Err(InstallerError::InvalidThumbprint(format!(
                "non-hex character `{}` in SHA-256 thumbprint",
                char::from(b)
            )));
        }
        hex_digits.push(b);
    }

    if hex_digits.len() != 64 {
        return Err(InstallerError::InvalidThumbprint(format!(
            "expected 64 hex characters for SHA-256 thumbprint, got {}",
            hex_digits.len()
        )));
    }

    let mut out = [0u8; 32];
    let (pairs, _) = hex_digits.as_chunks::<2>();
    for (idx, chunk) in pairs.iter().enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out[idx] = (hi << 4) | lo;
    }
    Ok(out)
}

const fn hex_nibble(b: u8) -> Result<u8, InstallerError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(InstallerError::InvalidThumbprint(String::new())),
    }
}

/// Formats a 32-byte SHA-256 digest as a lowercase 64-character hexadecimal string.
pub fn format_sha256_hex(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for &byte in digest {
        out.push(char::from(HEX[(byte >> 4) as usize]));
        out.push(char::from(HEX[(byte & 0x0F) as usize]));
    }
    out
}

/// Computes the SHA-256 digest of DER-encoded X.509 certificate bytes.
pub fn compute_cert_sha256(der_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(der_bytes);
    hasher.finalize().into()
}

/// Normalizes a DER or PEM certificate file payload into validated DER X.509 bytes.
pub fn parse_cert_bytes_to_der(raw_bytes: &[u8]) -> Result<Vec<u8>, InstallerError> {
    if raw_bytes.is_empty() {
        return Err(InstallerError::InvalidCertificate(
            "certificate file is empty".to_string(),
        ));
    }

    if let Ok(text) = core::str::from_utf8(raw_bytes) {
        if let Some(pem_der) = try_decode_pem_certificate(text)? {
            validate_der_sequence(&pem_der)?;
            return Ok(pem_der);
        }
    }

    validate_der_sequence(raw_bytes)?;
    Ok(raw_bytes.to_vec())
}

fn validate_der_sequence(der: &[u8]) -> Result<(), InstallerError> {
    if der.len() < 4 || der[0] != 0x30 {
        return Err(InstallerError::InvalidCertificate(
            "expected DER X.509 ASN.1 SEQUENCE (0x30)".to_string(),
        ));
    }
    let len_byte = der[1];
    let (header_len, content_len) = if (len_byte & 0x80) == 0 {
        (2usize, usize::from(len_byte))
    } else {
        let count = usize::from(len_byte & 0x7F);
        if count == 0 || count > 4 || der.len() < 2 + count {
            return Err(InstallerError::InvalidCertificate(
                "malformed ASN.1 DER length encoding".to_string(),
            ));
        }
        let mut val = 0usize;
        for &b in &der[2..2 + count] {
            val = (val << 8) | usize::from(b);
        }
        (2 + count, val)
    };

    if header_len.saturating_add(content_len) != der.len() {
        return Err(InstallerError::InvalidCertificate(format!(
            "DER certificate byte length mismatch: expected {}, got {}",
            header_len.saturating_add(content_len),
            der.len()
        )));
    }
    Ok(())
}

fn try_decode_pem_certificate(text: &str) -> Result<Option<Vec<u8>>, InstallerError> {
    const BEGIN_MARKER: &str = "-----BEGIN CERTIFICATE-----";
    const END_MARKER: &str = "-----END CERTIFICATE-----";

    let Some(start_idx) = text.find(BEGIN_MARKER) else {
        return Ok(None);
    };
    let after_begin = &text[start_idx + BEGIN_MARKER.len()..];
    let Some(end_idx) = after_begin.find(END_MARKER) else {
        return Err(InstallerError::InvalidCertificate(
            "unterminated PEM CERTIFICATE block".to_string(),
        ));
    };

    let b64_body: String = after_begin[..end_idx]
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let decoded = decode_base64(&b64_body)?;
    Ok(Some(decoded))
}

fn decode_base64(input: &str) -> Result<Vec<u8>, InstallerError> {
    if input.is_empty() || !input.len().is_multiple_of(4) {
        return Err(InstallerError::InvalidCertificate(
            "invalid PEM base64 length".to_string(),
        ));
    }

    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
    let (quads, _) = bytes.as_chunks::<4>();
    for (chunk_idx, chunk) in quads.iter().enumerate() {
        let is_last = (chunk_idx + 1) * 4 == bytes.len();
        let pad_count = chunk.iter().rev().take_while(|&&b| b == b'=').count();
        if pad_count > 2 || (pad_count > 0 && !is_last) {
            return Err(InstallerError::InvalidCertificate(
                "invalid PEM base64 padding".to_string(),
            ));
        }

        let a = b64_val(chunk[0])?;
        let b = b64_val(chunk[1])?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            b64_val(chunk[2])?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            b64_val(chunk[3])?
        };

        out.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            out.push((b << 4) | (c >> 2));
        }
        if chunk[3] != b'=' {
            out.push((c << 6) | d);
        }
    }
    Ok(out)
}

const fn b64_val(b: u8) -> Result<u8, InstallerError> {
    match b {
        b'A'..=b'Z' => Ok(b - b'A'),
        b'a'..=b'z' => Ok(b - b'a' + 26),
        b'0'..=b'9' => Ok(b - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(InstallerError::InvalidCertificate(String::new())),
    }
}

/// Loads a certificate file from `cert_path` and verifies its SHA-256 thumbprint matches `expected_sha256_hex`.
///
/// Always validates the expected hex thumbprint and compares the computed SHA-256 digest in pure Rust
/// before any certificate store operation may occur.
pub fn load_and_verify_cert(
    cert_path: &Path,
    expected_sha256_hex: &str,
) -> Result<VerifiedDevCert, InstallerError> {
    let expected_bytes = parse_sha256_hex(expected_sha256_hex)?;
    let raw = fs::read(cert_path)?;
    let der_bytes = parse_cert_bytes_to_der(&raw)?;
    verify_cert_der_bytes(der_bytes, &expected_bytes)
}

/// Verifies raw DER/PEM certificate bytes against an expected SHA-256 hex thumbprint.
pub fn verify_cert_bytes(
    raw_bytes: &[u8],
    expected_sha256_hex: &str,
) -> Result<VerifiedDevCert, InstallerError> {
    let expected_bytes = parse_sha256_hex(expected_sha256_hex)?;
    let der_bytes = parse_cert_bytes_to_der(raw_bytes)?;
    verify_cert_der_bytes(der_bytes, &expected_bytes)
}

fn verify_cert_der_bytes(
    der_bytes: Vec<u8>,
    expected_bytes: &[u8; 32],
) -> Result<VerifiedDevCert, InstallerError> {
    let actual_bytes = compute_cert_sha256(&der_bytes);
    let expected_hex = format_sha256_hex(expected_bytes);
    let actual_hex = format_sha256_hex(&actual_bytes);

    if &actual_bytes != expected_bytes {
        return Err(InstallerError::ThumbprintMismatch {
            expected: expected_hex,
            actual: actual_hex,
        });
    }

    Ok(VerifiedDevCert {
        der_bytes,
        sha256_bytes: actual_bytes,
        sha256_hex: actual_hex,
    })
}

/// Loads [`InstallState`] from `path`, returning an empty default state when absent.
pub fn load_install_state(path: &Path) -> Result<InstallState, InstallerError> {
    let raw = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(InstallState::default()),
        Err(err) => return Err(InstallerError::Io(err)),
    };
    let state: InstallState = serde_json::from_str(&raw)?;
    if state.schema_version != INSTALL_STATE_SCHEMA_VERSION {
        return Err(InstallerError::CorruptInstallState(format!(
            "unsupported schema_version {} (expected {INSTALL_STATE_SCHEMA_VERSION})",
            state.schema_version
        )));
    }
    Ok(state)
}

/// Atomically persists `state` to `path`, or removes the file if no tracked certs remain.
pub fn save_install_state_atomic(path: &Path, state: &InstallState) -> Result<(), InstallerError> {
    if state.schema_version != INSTALL_STATE_SCHEMA_VERSION {
        return Err(InstallerError::CorruptInstallState(format!(
            "unsupported schema_version {} (expected {INSTALL_STATE_SCHEMA_VERSION})",
            state.schema_version
        )));
    }

    if state.installed_dev_certs.is_empty() {
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(InstallerError::Io(err)),
        }
    }

    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            InstallerError::InvalidInput(format!("invalid install-state path {}", path.display()))
        })?;
    fs::create_dir_all(parent)?;

    let encoded = serde_json::to_string_pretty(state)?;
    // WHY: Staging in the destination directory guarantees atomic same-volume replacement via persist().
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(encoded.as_bytes())?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|err| InstallerError::Io(err.error))?;
    Ok(())
}

/// Records an installed development certificate thumbprint and store list idempotently in `state`.
pub fn record_installed_cert(
    state: &mut InstallState,
    sha256_hex: &str,
    stores: &[CertStoreLocation],
) {
    if let Some(existing) = state
        .installed_dev_certs
        .iter_mut()
        .find(|entry| entry.sha256.eq_ignore_ascii_case(sha256_hex))
    {
        for &store in stores {
            if !existing.stores.contains(&store) {
                existing.stores.push(store);
            }
        }
        return;
    }

    state.installed_dev_certs.push(InstalledCertRecord {
        sha256: sha256_hex.to_ascii_lowercase(),
        stores: stores.to_vec(),
    });
}

#[cfg(windows)]
mod win32 {
    use core::ffi::c_void;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::Security::Cryptography::{
        CertAddEncodedCertificateToStore, CertCloseStore, CertDeleteCertificateFromStore,
        CertDuplicateCertificateContext, CertEnumCertificatesInStore, CertFreeCertificateContext,
        CertOpenStore, CERT_CONTEXT, CERT_OPEN_STORE_FLAGS, CERT_QUERY_ENCODING_TYPE,
        CERT_STORE_ADD_REPLACE_EXISTING, CERT_STORE_OPEN_EXISTING_FLAG, CERT_STORE_PROV_SYSTEM_W,
        CERT_SYSTEM_STORE_LOCAL_MACHINE, HCERTSTORE, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
    };

    use super::{compute_cert_sha256, parse_sha256_hex, CertStoreLocation, VerifiedDevCert};
    use crate::InstallerError;

    struct OwnedCertStore(HCERTSTORE);

    impl Drop for OwnedCertStore {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` was opened via `CertOpenStore` and is closed exactly once on drop.
                unsafe {
                    let _ = CertCloseStore(Some(self.0), 0);
                }
            }
        }
    }

    fn open_local_machine_store(
        location: CertStoreLocation,
    ) -> Result<OwnedCertStore, InstallerError> {
        let wide_name: Vec<u16> = location
            .windows_store_name()
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        // SAFETY: `wide_name` is NUL-terminated UTF-16 pointing to a system store name under LocalMachine.
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                CERT_QUERY_ENCODING_TYPE(0),
                None,
                CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_LOCAL_MACHINE)
                    | CERT_STORE_OPEN_EXISTING_FLAG,
                Some(wide_name.as_ptr().cast::<c_void>()),
            )
        }
        .map_err(|err| InstallerError::Win32 {
            operation: "CertOpenStore",
            code: err.code().0 as u32,
        })?;
        Ok(OwnedCertStore(store))
    }

    /// Imports `cert` into `LocalMachine\Root` and `LocalMachine\TrustedPublisher`.
    pub fn install_cert_to_local_machine_stores(
        cert: &VerifiedDevCert,
    ) -> Result<Vec<CertStoreLocation>, InstallerError> {
        let stores = CertStoreLocation::default_dev_stores();
        for store_loc in stores {
            let store = open_local_machine_store(store_loc)?;
            // SAFETY: `store.0` is an open HCERTSTORE and `cert.der_bytes` is valid DER-encoded X.509 data.
            unsafe {
                CertAddEncodedCertificateToStore(
                    Some(store.0),
                    X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                    &cert.der_bytes,
                    CERT_STORE_ADD_REPLACE_EXISTING,
                    None,
                )
            }
            .map_err(|err| InstallerError::Win32 {
                operation: "CertAddEncodedCertificateToStore",
                code: err.code().0 as u32,
            })?;
        }
        Ok(stores.to_vec())
    }

    /// Removes only certificates matching `sha256_hex` from the specified LocalMachine stores.
    pub fn remove_cert_from_local_machine_stores(
        sha256_hex: &str,
        stores: &[CertStoreLocation],
    ) -> Result<usize, InstallerError> {
        let target_digest = parse_sha256_hex(sha256_hex)?;
        let mut removed = 0usize;

        for &store_loc in stores {
            struct CertMatchList(Vec<*const CERT_CONTEXT>);

            impl Drop for CertMatchList {
                fn drop(&mut self) {
                    for ctx in self.0.drain(..) {
                        if !ctx.is_null() {
                            // SAFETY: Any unconsumed context in `self.0` was returned by
                            // `CertDuplicateCertificateContext` and must be freed on early error exit.
                            unsafe {
                                let _ = CertFreeCertificateContext(Some(ctx));
                            }
                        }
                    }
                }
            }

            let store = open_local_machine_store(store_loc)?;
            let mut prev: *const CERT_CONTEXT = core::ptr::null();
            let mut matches = CertMatchList(Vec::new());

            loop {
                // SAFETY: `store.0` is valid and `prev` is either null or the previous `CertEnumCertificatesInStore` cursor.
                let next = unsafe { CertEnumCertificatesInStore(store.0, Some(prev)) };
                if next.is_null() {
                    break;
                }
                // SAFETY: `next` is a non-null `CERT_CONTEXT` returned by `CertEnumCertificatesInStore`.
                let ctx = unsafe { &*next };
                if !ctx.pbCertEncoded.is_null() && ctx.cbCertEncoded > 0 {
                    // SAFETY: `pbCertEncoded` points to `cbCertEncoded` valid bytes owned by the context.
                    let der = unsafe {
                        core::slice::from_raw_parts(ctx.pbCertEncoded, ctx.cbCertEncoded as usize)
                    };
                    if compute_cert_sha256(der) == target_digest {
                        // SAFETY: Duplicating increments the refcount so enumeration continues cleanly.
                        let dup = unsafe { CertDuplicateCertificateContext(Some(next)) };
                        if !dup.is_null() {
                            matches.0.push(dup);
                        }
                    }
                }
                prev = next;
            }

            while let Some(ctx_ptr) = matches.0.pop() {
                // SAFETY: `CertDeleteCertificateFromStore` consumes and frees `ctx_ptr`.
                let ok = unsafe { CertDeleteCertificateFromStore(ctx_ptr) };
                if ok.is_ok() {
                    removed += 1;
                } else {
                    // SAFETY: `GetLastError` retrieves the thread-local Win32 error immediately after failure.
                    let err = unsafe { GetLastError() };
                    return Err(InstallerError::Win32 {
                        operation: "CertDeleteCertificateFromStore",
                        code: err.0,
                    });
                }
            }
        }

        // WHY: Keep unused import check satisfied if PCWSTR is only referenced in sibling helpers.
        let _ = PCWSTR::null();
        Ok(removed)
    }
}

#[cfg(windows)]
pub use win32::{install_cert_to_local_machine_stores, remove_cert_from_local_machine_stores};
