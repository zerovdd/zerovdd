use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use zerovdd_installer::cert::{
    compute_cert_sha256, format_sha256_hex, load_and_verify_cert, load_install_state,
    parse_cert_bytes_to_der, parse_sha256_hex, save_install_state_atomic, verify_cert_bytes,
    CertStoreLocation, InstallState,
};
use zerovdd_installer::device_node::{
    encode_hardware_id_multi_sz, matches_zerovdd_hardware_id, parse_multi_sz_utf16, DeviceNodeInfo,
    ServiceStopOutcome, HARDWARE_ID, SERVICE_NAME,
};
use zerovdd_installer::driver_store::{
    classify_di_install_hresult, decode_inf_bytes, is_driver_store_isolated_path,
    is_zerovdd_inf_content, parse_driver_ver_value, parse_oem_inf_number, sort_oem_inf_entries,
    DriverPackageStatus, DriverStageResult,
};
use zerovdd_installer::{
    install_with_backend, status_with_backend, uninstall_with_backend, upgrade_with_backend,
    DevCertOption, InstallOptions, InstallerBackend, InstallerError, UninstallOptions,
};

fn build_synthetic_x509_der(serial_byte: u8) -> Vec<u8> {
    let inner_payload = [
        0xA0,
        0x03,
        0x02,
        0x01,
        0x02,
        0x02,
        0x01,
        serial_byte,
        0x30,
        0x0D,
        0x06,
        0x09,
        0x2A,
        0x86,
        0x48,
        0x86,
        0xF7,
        0x0D,
        0x01,
        0x01,
        0x0B,
        0x05,
        0x00,
        0x30,
        0x13,
        0x31,
        0x11,
        0x30,
        0x0F,
        0x06,
        0x03,
        0x55,
        0x04,
        0x03,
        0x0C,
        0x08,
        b'r',
        b'u',
        b's',
        b't',
        b'-',
        b'v',
        b'd',
        b'd',
    ];
    let tbs_len = inner_payload.len();
    let mut cert_body = Vec::new();
    cert_body.push(0x30);
    cert_body.push(tbs_len as u8);
    cert_body.extend_from_slice(&inner_payload);
    cert_body.extend_from_slice(&[
        0x30, 0x03, 0x05, 0x00, 0x00, 0x03, 0x04, 0x00, 0xAA, 0xBB, 0xCC,
    ]);

    let total_len = cert_body.len();
    let mut der = Vec::with_capacity(total_len + 4);
    der.push(0x30);
    if total_len < 128 {
        der.push(total_len as u8);
    } else {
        der.push(0x81);
        der.push(total_len as u8);
    }
    der.extend_from_slice(&cert_body);
    der
}

fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(char::from(TABLE[(b0 >> 2) as usize]));
        out.push(char::from(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize]));
        if chunk.len() > 1 {
            out.push(char::from(TABLE[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize]));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(TABLE[(b2 & 0x3F) as usize]));
        } else {
            out.push('=');
        }
    }
    out
}

fn wrap_pem_cert(der: &[u8]) -> String {
    let b64 = encode_base64(der);
    format!("-----BEGIN CERTIFICATE-----\n{b64}\n-----END CERTIFICATE-----\n")
}

#[derive(Debug, Default)]
struct FakeBackendState {
    calls: Vec<String>,
    service_running: bool,
    service_installed: bool,
    nodes: Vec<DeviceNodeInfo>,
    oem_infs: Vec<String>,
    installed_certs: BTreeSet<(CertStoreLocation, String)>,
    force_stage_reboot: bool,
    force_node_reboot: bool,
    force_uninstall_reboot: bool,
    last_force_reinstall: Option<bool>,
}

struct FakeInstallerBackend {
    state: RefCell<FakeBackendState>,
}

impl FakeInstallerBackend {
    fn new(state: FakeBackendState) -> Self {
        Self {
            state: RefCell::new(state),
        }
    }
}

impl InstallerBackend for FakeInstallerBackend {
    fn stop_service_if_running(
        &self,
        service_name: &str,
    ) -> Result<ServiceStopOutcome, InstallerError> {
        let mut st = self.state.borrow_mut();
        st.calls.push(format!("stop_service:{service_name}"));
        if !st.service_installed {
            return Ok(ServiceStopOutcome::NotInstalled);
        }
        if st.service_running {
            st.service_running = false;
            Ok(ServiceStopOutcome::Stopped)
        } else {
            Ok(ServiceStopOutcome::AlreadyStopped)
        }
    }

    fn install_dev_cert(
        &self,
        cert: &zerovdd_installer::cert::VerifiedDevCert,
    ) -> Result<Vec<CertStoreLocation>, InstallerError> {
        let mut st = self.state.borrow_mut();
        st.calls
            .push(format!("install_dev_cert:{}", cert.sha256_hex));
        let stores = CertStoreLocation::default_dev_stores();
        for store in stores {
            st.installed_certs.insert((store, cert.sha256_hex.clone()));
        }
        Ok(stores.to_vec())
    }

    fn remove_dev_cert(
        &self,
        sha256_hex: &str,
        stores: &[CertStoreLocation],
    ) -> Result<usize, InstallerError> {
        let mut st = self.state.borrow_mut();
        let lower = sha256_hex.to_ascii_lowercase();
        st.calls.push(format!("remove_dev_cert:{lower}"));
        let mut removed = 0usize;
        for &store in stores {
            if st.installed_certs.remove(&(store, lower.clone())) {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn enumerate_device_nodes(&self) -> Result<Vec<DeviceNodeInfo>, InstallerError> {
        let mut st = self.state.borrow_mut();
        st.calls.push("enumerate_device_nodes".to_string());
        Ok(st.nodes.clone())
    }

    fn stage_and_install_inf(
        &self,
        _inf_path: &Path,
        force_reinstall: bool,
    ) -> Result<DriverStageResult, InstallerError> {
        let mut st = self.state.borrow_mut();
        st.calls
            .push(format!("stage_and_install_inf:force={force_reinstall}"));
        st.last_force_reinstall = Some(force_reinstall);
        if !st.oem_infs.iter().any(|n| n == "oem42.inf") {
            st.oem_infs.push("oem42.inf".to_string());
        }
        Ok(DriverStageResult {
            oem_inf_name: Some("oem42.inf".to_string()),
            driver_store_path: Some(PathBuf::from(
                r"C:\Windows\System32\DriverStore\FileRepository\zerovdd.inf_amd64_01234567\zerovdd.inf",
            )),
            reboot_required: st.force_stage_reboot,
        })
    }

    fn create_device_node(&self) -> Result<(String, bool), InstallerError> {
        let mut st = self.state.borrow_mut();
        let next_idx = st.nodes.len();
        let instance_id = format!(r"ROOT\DISPLAY\{next_idx:04}");
        st.calls.push(format!("create_device_node:{instance_id}"));
        st.nodes.push(DeviceNodeInfo {
            instance_id: instance_id.clone(),
            hardware_ids: vec![HARDWARE_ID.to_string()],
            present: true,
            has_problem: false,
            problem_code: 0,
            status_flags: 0x0180_200A,
        });
        Ok((instance_id, st.force_node_reboot))
    }

    fn uninstall_device_nodes(&self) -> Result<(Vec<String>, bool), InstallerError> {
        let mut st = self.state.borrow_mut();
        st.calls.push("uninstall_device_nodes".to_string());
        let removed: Vec<String> = st.nodes.drain(..).map(|n| n.instance_id).collect();
        Ok((removed, st.force_uninstall_reboot))
    }

    fn uninstall_oem_infs(&self) -> Result<(Vec<String>, bool), InstallerError> {
        let mut st = self.state.borrow_mut();
        st.calls.push("uninstall_oem_infs".to_string());
        let removed = std::mem::take(&mut st.oem_infs);
        Ok((removed, false))
    }

    fn query_driver_status(&self) -> Result<DriverPackageStatus, InstallerError> {
        let st = self.state.borrow();
        if let Some(oem) = st.oem_infs.last() {
            Ok(DriverPackageStatus {
                installed: true,
                oem_inf_name: Some(oem.clone()),
                oem_inf_path: Some(PathBuf::from(format!(r"C:\Windows\INF\{oem}"))),
                driver_store_path: Some(PathBuf::from(
                    r"C:\Windows\System32\DriverStore\FileRepository\zerovdd.inf_amd64_01234567\zerovdd.inf",
                )),
                dirid_13_isolated: true,
                driver_version: Some("0.1.0.0".to_string()),
                driver_date: Some("11/24/2026".to_string()),
                provider_name: Some("zerovdd".to_string()),
                signer_name: Some("ZeroVDD Development Test Signing".to_string()),
            })
        } else {
            Ok(DriverPackageStatus::default())
        }
    }
}

fn create_temp_inf(dir: &TempDir) -> PathBuf {
    let inf_path = dir.path().join("zerovdd.inf");
    fs::write(
        &inf_path,
        "[Version]\nSignature=\"$Windows NT$\"\n[Standard.NTamd64]\n%Dev%=Inst, Root\\ZeroVdd\n[UMDriverCopy]\nzerovdd.dll\n",
    )
    .expect("write temp inf");
    inf_path
}

#[test]
fn sha256_thumbprint_normalization_match_and_mismatch_rejection() {
    let der = build_synthetic_x509_der(0x42);
    let digest = compute_cert_sha256(&der);
    let lower_hex = format_sha256_hex(&digest);
    let upper_colon_hex: String = lower_hex
        .as_bytes()
        .chunks(2)
        .map(|pair| core::str::from_utf8(pair).unwrap().to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join(":");

    assert_eq!(parse_sha256_hex(&lower_hex).unwrap(), digest);
    assert_eq!(
        parse_sha256_hex(&format!("sha256:{upper_colon_hex}")).unwrap(),
        digest
    );
    assert!(matches!(
        parse_sha256_hex("deadbeef"),
        Err(InstallerError::InvalidThumbprint(_))
    ));
    assert!(matches!(
        parse_sha256_hex(&"z".repeat(64)),
        Err(InstallerError::InvalidThumbprint(_))
    ));

    let verified_der = verify_cert_bytes(&der, &upper_colon_hex).unwrap();
    assert_eq!(verified_der.sha256_hex, lower_hex);

    let pem = wrap_pem_cert(&der);
    let parsed_pem_der = parse_cert_bytes_to_der(pem.as_bytes()).unwrap();
    assert_eq!(parsed_pem_der, der);

    let dir = TempDir::new().unwrap();
    let cert_path = dir.path().join("dev.cer");
    fs::write(&cert_path, &der).unwrap();
    let loaded = load_and_verify_cert(&cert_path, &lower_hex).unwrap();
    assert_eq!(loaded.der_bytes, der);

    let wrong_hex = "0".repeat(64);
    let err = load_and_verify_cert(&cert_path, &wrong_hex).unwrap_err();
    assert!(
        matches!(err, InstallerError::ThumbprintMismatch { .. }),
        "expected ThumbprintMismatch, got {err:?}"
    );
}

#[test]
fn thumbprint_mismatch_aborts_before_touching_cert_store_or_driver_store() {
    let dir = TempDir::new().unwrap();
    let inf_path = create_temp_inf(&dir);
    let cert_path = dir.path().join("dev.cer");
    let der = build_synthetic_x509_der(0x07);
    fs::write(&cert_path, &der).unwrap();

    let state_path = dir.path().join("install-state.json");
    let backend = FakeInstallerBackend::new(FakeBackendState {
        service_installed: true,
        service_running: true,
        ..Default::default()
    });

    let opts = InstallOptions {
        dev_cert: Some(DevCertOption {
            cert_path,
            expected_sha256: "f".repeat(64),
        }),
        state_path: Some(state_path.clone()),
    };

    let err = install_with_backend(&backend, &inf_path, &opts).unwrap_err();
    assert!(matches!(err, InstallerError::ThumbprintMismatch { .. }));

    let st = backend.state.borrow();
    assert!(
        st.calls.is_empty(),
        "no backend calls may happen on thumbprint mismatch, got: {:?}",
        st.calls
    );
    assert!(st.service_running, "service must remain untouched");
    assert!(
        !state_path.exists(),
        "install-state.json must not be created"
    );
}

#[test]
fn idempotent_install_and_upgrade_create_zero_duplicate_device_nodes() {
    let dir = TempDir::new().unwrap();
    let inf_path = create_temp_inf(&dir);
    let cert_path = dir.path().join("dev.pem");
    let der = build_synthetic_x509_der(0x11);
    fs::write(&cert_path, wrap_pem_cert(&der)).unwrap();
    let expected_hex = format_sha256_hex(&compute_cert_sha256(&der));
    let state_path = dir.path().join("install-state.json");

    let backend = FakeInstallerBackend::new(FakeBackendState {
        service_installed: true,
        service_running: true,
        ..Default::default()
    });

    let opts = InstallOptions {
        dev_cert: Some(DevCertOption {
            cert_path,
            expected_sha256: expected_hex.clone(),
        }),
        state_path: Some(state_path.clone()),
    };

    let first = install_with_backend(&backend, &inf_path, &opts).unwrap();
    assert!(first.created_device_node);
    assert_eq!(first.device_instance_ids.len(), 1);
    assert_eq!(first.service_stop, ServiceStopOutcome::Stopped);
    assert_eq!(
        first.installed_dev_cert_sha256.as_deref(),
        Some(expected_hex.as_str())
    );
    assert!(!first.reboot_required);
    assert_eq!(backend.state.borrow().nodes.len(), 1);
    assert_eq!(backend.state.borrow().last_force_reinstall, Some(false));

    backend.state.borrow_mut().calls.clear();
    backend.state.borrow_mut().service_running = true;
    backend.state.borrow_mut().force_stage_reboot = true;

    let upgraded = upgrade_with_backend(&backend, &inf_path, &opts).unwrap();
    assert!(!upgraded.created_device_node);
    assert_eq!(upgraded.device_instance_ids.len(), 1);
    assert_eq!(upgraded.service_stop, ServiceStopOutcome::Stopped);
    assert!(upgraded.reboot_required);
    assert_eq!(backend.state.borrow().nodes.len(), 1);
    assert_eq!(backend.state.borrow().last_force_reinstall, Some(true));

    let calls = &backend.state.borrow().calls;
    let stop_idx = calls
        .iter()
        .position(|c| c == &format!("stop_service:{SERVICE_NAME}"))
        .expect("stop_service called");
    let stage_idx = calls
        .iter()
        .position(|c| c.starts_with("stage_and_install_inf"))
        .expect("stage_and_install_inf called");
    assert!(
        stop_idx < stage_idx,
        "service must stop before staging driver INF: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("create_device_node")),
        "upgrade must never create a second device node: {calls:?}"
    );

    let persisted = load_install_state(&state_path).unwrap();
    assert_eq!(persisted.installed_dev_certs.len(), 1);
    assert_eq!(persisted.installed_dev_certs[0].sha256, expected_hex);
    assert_eq!(
        persisted.installed_dev_certs[0].stores,
        vec![
            CertStoreLocation::LocalMachineRoot,
            CertStoreLocation::LocalMachineTrustedPublisher
        ]
    );
}

#[test]
fn uninstall_removes_only_tracked_certs_and_propagates_reboot_flag() {
    let dir = TempDir::new().unwrap();
    let inf_path = create_temp_inf(&dir);
    let cert_path = dir.path().join("dev.cer");
    let der = build_synthetic_x509_der(0x22);
    fs::write(&cert_path, &der).unwrap();
    let expected_hex = format_sha256_hex(&compute_cert_sha256(&der));
    let state_path = dir.path().join("install-state.json");

    let unrelated_cert = "a".repeat(64);
    let mut initial_certs = BTreeSet::new();
    initial_certs.insert((CertStoreLocation::LocalMachineRoot, unrelated_cert.clone()));
    initial_certs.insert((
        CertStoreLocation::LocalMachineTrustedPublisher,
        unrelated_cert.clone(),
    ));

    let backend = FakeInstallerBackend::new(FakeBackendState {
        service_installed: true,
        service_running: false,
        installed_certs: initial_certs,
        ..Default::default()
    });

    let install_opts = InstallOptions {
        dev_cert: Some(DevCertOption {
            cert_path,
            expected_sha256: expected_hex.clone(),
        }),
        state_path: Some(state_path.clone()),
    };
    install_with_backend(&backend, &inf_path, &install_opts).unwrap();

    let status_before = status_with_backend(&backend, Some(&state_path)).unwrap();
    assert!(status_before.driver.installed);
    assert!(status_before.driver.dirid_13_isolated);
    assert_eq!(
        status_before.driver.oem_inf_name.as_deref(),
        Some("oem42.inf")
    );
    assert_eq!(status_before.device_nodes.len(), 1);
    assert_eq!(status_before.tracked_dev_certs.len(), 1);

    backend.state.borrow_mut().service_running = true;
    backend.state.borrow_mut().force_uninstall_reboot = true;

    let uninstall_out = uninstall_with_backend(
        &backend,
        &UninstallOptions {
            state_path: Some(state_path.clone()),
        },
    )
    .unwrap();
    assert_eq!(uninstall_out.service_stop, ServiceStopOutcome::Stopped);
    assert_eq!(uninstall_out.removed_device_nodes.len(), 1);
    assert_eq!(uninstall_out.removed_oem_infs, vec!["oem42.inf"]);
    assert_eq!(uninstall_out.removed_dev_cert_count, 2);
    assert!(uninstall_out.reboot_required);
    assert!(
        !state_path.exists(),
        "empty install-state.json must be cleaned up"
    );

    let st = backend.state.borrow();
    assert!(st.nodes.is_empty());
    assert!(st.oem_infs.is_empty());
    assert!(
        !st.installed_certs
            .contains(&(CertStoreLocation::LocalMachineRoot, expected_hex.clone())),
        "tracked dev cert must be removed from Root"
    );
    assert!(
        st.installed_certs
            .contains(&(CertStoreLocation::LocalMachineRoot, unrelated_cert.clone())),
        "unrelated pre-existing Root cert must NEVER be removed"
    );
    assert!(
        st.installed_certs.contains(&(
            CertStoreLocation::LocalMachineTrustedPublisher,
            unrelated_cert
        )),
        "unrelated pre-existing TrustedPublisher cert must NEVER be removed"
    );
}

#[test]
fn driver_store_and_hardware_id_helpers_validate_cleanly() {
    let raw_bytes = encode_hardware_id_multi_sz(HARDWARE_ID);
    let (word_pairs, _) = raw_bytes.as_chunks::<2>();
    let words: Vec<u16> = word_pairs.iter().map(|p| u16::from_le_bytes(*p)).collect();
    let parsed = parse_multi_sz_utf16(&words);
    assert_eq!(parsed, vec![HARDWARE_ID.to_string()]);
    assert!(matches_zerovdd_hardware_id(&[String::from(
        r"root\zerovdd"
    )]));
    assert!(!matches_zerovdd_hardware_id(&[String::from(
        r"PCI\VEN_10DE"
    )]));

    assert_eq!(
        parse_driver_ver_value("11/24/2026,0.1.0.0"),
        (Some("11/24/2026".to_string()), Some("0.1.0.0".to_string()))
    );
    assert!(is_zerovdd_inf_content(
        "%Dev% = ZeroVdd_Install, Root\\ZeroVdd\nCopyFiles = UMDriverCopy\nzerovdd.dll"
    ));
    assert!(!is_zerovdd_inf_content("unrelated display inf"));

    assert!(is_driver_store_isolated_path(
        Path::new(
            r"C:\Windows\System32\DriverStore\FileRepository\zerovdd.inf_amd64_abc\zerovdd.inf"
        ),
        true
    ));
    assert!(!is_driver_store_isolated_path(
        Path::new(r"C:\Windows\System32\drivers\UMDF\zerovdd.dll"),
        true
    ));
    assert!(!is_driver_store_isolated_path(
        Path::new(
            r"C:\Windows\System32\DriverStore\FileRepository\zerovdd.inf_amd64_abc\zerovdd.inf"
        ),
        false
    ));

    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("install-state.json");
    let mut state = InstallState::default();
    save_install_state_atomic(&state_path, &state).unwrap();
    assert!(!state_path.exists());
    state.schema_version = 99;
    assert!(matches!(
        save_install_state_atomic(&state_path, &state),
        Err(InstallerError::CorruptInstallState(_))
    ));
}

#[test]
fn decodes_utf16_bom_inf_and_sorts_oem_infs_numerically() {
    let inf_text = "[Standard.NTamd64]\n%Dev% = ZeroVdd_Install, Root\\ZeroVdd\nzerovdd.dll\n";
    let mut utf16le = vec![0xFFu8, 0xFE];
    for u in inf_text.encode_utf16() {
        utf16le.extend_from_slice(&u.to_le_bytes());
    }
    let decoded_le = decode_inf_bytes(&utf16le);
    assert!(is_zerovdd_inf_content(&decoded_le));

    let mut utf16be = vec![0xFEu8, 0xFF];
    for u in inf_text.encode_utf16() {
        utf16be.extend_from_slice(&u.to_be_bytes());
    }
    let decoded_be = decode_inf_bytes(&utf16be);
    assert!(is_zerovdd_inf_content(&decoded_be));

    let mut utf8_bom = vec![0xEFu8, 0xBB, 0xBF];
    utf8_bom.extend_from_slice(inf_text.as_bytes());
    assert!(is_zerovdd_inf_content(&decode_inf_bytes(&utf8_bom)));

    assert_eq!(parse_oem_inf_number("oem9.inf"), Some(9));
    assert_eq!(parse_oem_inf_number("OEM10.INF"), Some(10));
    assert_eq!(parse_oem_inf_number("other.inf"), None);

    let mut entries = vec![
        ("oem10.inf".to_string(), 10),
        ("oem2.inf".to_string(), 2),
        ("oem42.inf".to_string(), 42),
        ("oem9.inf".to_string(), 9),
    ];
    sort_oem_inf_entries(&mut entries);
    let ordered: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        ordered,
        vec!["oem2.inf", "oem9.inf", "oem10.inf", "oem42.inf"]
    );
}

#[test]
fn di_install_hresult_treats_no_more_items_as_idempotent_success() {
    assert_eq!(classify_di_install_hresult(0x8007_0103), Ok(false));
    assert_eq!(classify_di_install_hresult(0x8007_0BC2), Ok(true));
    assert_eq!(classify_di_install_hresult(0x8007_0005), Err(0x8007_0005));
}
