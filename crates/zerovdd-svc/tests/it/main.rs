use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier, Mutex};
use std::thread;

use tempfile::TempDir;
use zerovdd_client::device::ControlClient;
use zerovdd_client::gpu::GpuAdapterInfo;
use zerovdd_client::topology::{
    save_snapshot_atomic, AdapterLuid, DisplayPathRecord, DisplayRotation, DisplayScaling,
    DisplayScanlineOrdering, TopologySnapshot, TOPOLOGY_SNAPSHOT_SCHEMA_VERSION,
};
use zerovdd_client::{monitor_guid_for_name, ClientError};
use zerovdd_config::{ConfigV1, PersistentDisplayConfig, SunshineTopologyMode};
use zerovdd_proto::ipc::{
    decode_frame, encode_frame, DisplayAddRequest, DisplayOrientation, DisplayRemoveRequest,
    DisplaySetModeRequest, IpcEnvelope, IpcRequest, IpcResponse, StartSessionRequest,
    StopSessionRequest, SwitchGpuRequest, TopologyMode, IPC_PROTOCOL_VERSION,
};
use zerovdd_proto::types::{
    AddDisplayRequest, AddDisplayResponse, DisplayInfo, DriverStatusResponse, ListDisplaysResponse,
    ProtocolVersion, RemoveDisplayRequest, RenewLeaseRequest, SetRenderAdapterRequest,
    UpdateDisplayModeRequest, UpdateDisplayModeResponse, MAX_LIST_DISPLAYS,
    REMOVE_FLAG_BY_TARGET_ID, UPDATE_MODE_FLAG_BY_TARGET_ID,
};
use zerovdd_svc::console_launcher::{
    resolve_sibling_cli_path, ApplyCcdPayload, ConsoleCcdRunner, LauncherError, RestoreCcdReport,
    SetModeCcdPayload,
};
use zerovdd_svc::persistent::HEADLESS_STANDBY_CLIENT_ID;
use zerovdd_svc::pipe::{handle_pipe_stream, IPC_PIPE_SDDL};
use zerovdd_svc::session::{
    ConsoleSessionEvent, DriverProvider, GpuEnumerator, ProcessWatcher, ServiceCoordinator,
    SessionError, StaticDriverProvider, WatchHandle,
};

#[derive(Debug, Clone)]
struct FakeDisplayEntry {
    guid: [u8; 16],
    luid: AdapterLuid,
    target_id: u32,
    connector_index: u32,
    req: AddDisplayRequest,
}

struct FakeDriverState {
    next_target_id: u32,
    displays: Vec<FakeDisplayEntry>,
    add_calls: Vec<AddDisplayRequest>,
    remove_calls: Vec<RemoveDisplayRequest>,
    update_mode_calls: Vec<UpdateDisplayModeRequest>,
    fail_next_add: bool,
    fail_next_add_err: Option<ClientError>,
    fail_get_version: bool,
}

struct FakeDriver {
    state: Mutex<FakeDriverState>,
}

impl FakeDriver {
    fn new() -> Self {
        Self {
            state: Mutex::new(FakeDriverState {
                next_target_id: 100,
                displays: Vec::new(),
                add_calls: Vec::new(),
                remove_calls: Vec::new(),
                update_mode_calls: Vec::new(),
                fail_next_add: false,
                fail_next_add_err: None,
                fail_get_version: false,
            }),
        }
    }

    fn set_fail_next_add(&self, fail: bool) {
        self.state.lock().unwrap().fail_next_add = fail;
    }

    fn set_fail_next_add_err(&self, err: ClientError) {
        self.state.lock().unwrap().fail_next_add_err = Some(err);
    }

    fn set_fail_get_version(&self, fail: bool) {
        self.state.lock().unwrap().fail_get_version = fail;
    }

    fn active_count(&self) -> usize {
        self.state.lock().unwrap().displays.len()
    }

    fn active_guids(&self) -> Vec<[u8; 16]> {
        self.state
            .lock()
            .unwrap()
            .displays
            .iter()
            .map(|d| d.guid)
            .collect()
    }
}

impl ControlClient for FakeDriver {
    fn get_version(&self) -> Result<ProtocolVersion, ClientError> {
        if self.state.lock().unwrap().fail_get_version {
            return Err(ClientError::Win32 {
                operation: "DeviceIoControl(IOCTL_GET_VERSION)",
                code: 0x8007_0006,
            });
        }
        Ok(ProtocolVersion::CURRENT)
    }

    fn add_display(&self, request: &AddDisplayRequest) -> Result<AddDisplayResponse, ClientError> {
        let mut guard = self.state.lock().unwrap();
        guard.add_calls.push(*request);
        if let Some(err) = guard.fail_next_add_err.take() {
            return Err(err);
        }
        if guard.fail_next_add {
            guard.fail_next_add = false;
            return Err(ClientError::DeviceNotFound(
                "simulated control device disconnect".to_string(),
            ));
        }
        if let Some(existing) = guard
            .displays
            .iter_mut()
            .find(|d| d.guid == request.monitor_guid)
        {
            existing.req = *request;
            return Ok(AddDisplayResponse {
                adapter_luid_low: existing.luid.low,
                adapter_luid_high: existing.luid.high,
                target_id: existing.target_id,
                connector_index: existing.connector_index,
                created_new: 0,
                reserved: [0; 3],
            });
        }

        let target_id = guard.next_target_id;
        guard.next_target_id += 1;
        let connector_index = guard.displays.len() as u32;
        let luid = AdapterLuid::new(0xABCD, 0);
        guard.displays.push(FakeDisplayEntry {
            guid: request.monitor_guid,
            luid,
            target_id,
            connector_index,
            req: *request,
        });
        Ok(AddDisplayResponse {
            adapter_luid_low: luid.low,
            adapter_luid_high: luid.high,
            target_id,
            connector_index,
            created_new: 1,
            reserved: [0; 3],
        })
    }

    fn remove_display(&self, request: &RemoveDisplayRequest) -> Result<(), ClientError> {
        let mut guard = self.state.lock().unwrap();
        guard.remove_calls.push(*request);
        let before = guard.displays.len();
        if (request.flags & REMOVE_FLAG_BY_TARGET_ID) != 0 {
            guard.displays.retain(|d| d.target_id != request.target_id);
        } else {
            guard.displays.retain(|d| d.guid != request.monitor_guid);
        }
        if guard.displays.len() == before {
            return Err(ClientError::MonitorNotFound);
        }
        Ok(())
    }

    fn list_displays(&self) -> Result<ListDisplaysResponse, ClientError> {
        let guard = self.state.lock().unwrap();
        let mut resp = ListDisplaysResponse::empty();
        resp.count = guard.displays.len().min(MAX_LIST_DISPLAYS) as u32;
        resp.total_active = guard.displays.len() as u32;
        for (idx, d) in guard.displays.iter().take(MAX_LIST_DISPLAYS).enumerate() {
            let mut info = DisplayInfo::empty();
            info.monitor_guid = d.guid;
            info.adapter_luid_low = d.luid.low;
            info.adapter_luid_high = d.luid.high;
            info.target_id = d.target_id;
            info.connector_index = d.connector_index;
            info.width = d.req.width;
            info.height = d.req.height;
            info.refresh_num = d.req.refresh_num;
            info.refresh_den = d.req.refresh_den;
            info.hdr_profile = d.req.hdr_profile;
            info.ownership_mode = d.req.ownership_mode;
            info.client_name_len = d.req.client_name_len;
            info.client_name = d.req.client_name;
            info.active = 1;
            resp.displays[idx] = info;
        }
        Ok(resp)
    }

    fn set_render_adapter(&self, _request: &SetRenderAdapterRequest) -> Result<(), ClientError> {
        Ok(())
    }

    fn renew_lease(&self, _request: &RenewLeaseRequest) -> Result<(), ClientError> {
        Ok(())
    }

    fn update_display_mode(
        &self,
        request: &UpdateDisplayModeRequest,
    ) -> Result<UpdateDisplayModeResponse, ClientError> {
        let mut guard = self.state.lock().unwrap();
        guard.update_mode_calls.push(*request);
        let matched = guard.displays.iter_mut().find(|d| {
            if (request.flags & UPDATE_MODE_FLAG_BY_TARGET_ID) != 0 {
                d.target_id == request.target_id
            } else {
                d.guid == request.monitor_guid
            }
        });
        let Some(display) = matched else {
            return Err(ClientError::MonitorNotFound);
        };
        display.req.width = request.width;
        display.req.height = request.height;
        display.req.refresh_num = request.refresh_num;
        display.req.refresh_den = request.refresh_den;
        display.req.hdr_profile = request.hdr_profile;
        Ok(UpdateDisplayModeResponse {
            adapter_luid_low: display.luid.low,
            adapter_luid_high: display.luid.high,
            target_id: display.target_id,
            connector_index: display.connector_index,
            mode_count: 10,
        })
    }

    fn get_status(&self) -> Result<DriverStatusResponse, ClientError> {
        let guard = self.state.lock().unwrap();
        Ok(DriverStatusResponse {
            protocol_version: ProtocolVersion::CURRENT,
            total_arrivals: guard.add_calls.len() as u64,
            total_departures: guard.remove_calls.len() as u64,
            uptime_secs: 1,
            render_adapter_luid_low: 0xABCD,
            render_adapter_luid_high: 0,
            active_display_count: guard.displays.len() as u32,
            max_displays: 16,
            adapter_ready: 1,
            hdr_capable: 1,
            last_gpu_switch_failed: 0,
            reserved: [0; 5],
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LauncherOp {
    Snapshot(PathBuf),
    Apply(ApplyCcdPayload),
    SetMode(SetModeCcdPayload),
    Restore(PathBuf, Option<(AdapterLuid, u32)>),
}

struct FakeLauncher {
    session_id: Mutex<Option<u32>>,
    has_active_physical: Mutex<bool>,
    ops: Mutex<Vec<LauncherOp>>,
    snapshot_counter: Mutex<u64>,
    restore_notify: Mutex<Option<mpsc::Sender<RestoreCcdReport>>>,
    fail_next_restore: Mutex<bool>,
    fail_next_snapshot: Mutex<bool>,
    fail_next_apply: Mutex<bool>,
    fail_next_set_mode: Mutex<bool>,
}

impl FakeLauncher {
    fn new(session_id: Option<u32>) -> Self {
        Self {
            session_id: Mutex::new(session_id),
            has_active_physical: Mutex::new(true),
            ops: Mutex::new(Vec::new()),
            snapshot_counter: Mutex::new(1),
            restore_notify: Mutex::new(None),
            fail_next_restore: Mutex::new(false),
            fail_next_snapshot: Mutex::new(false),
            fail_next_apply: Mutex::new(false),
            fail_next_set_mode: Mutex::new(false),
        }
    }

    fn set_fail_next_restore(&self, fail: bool) {
        *self.fail_next_restore.lock().unwrap() = fail;
    }

    fn set_fail_next_snapshot(&self, fail: bool) {
        *self.fail_next_snapshot.lock().unwrap() = fail;
    }

    fn set_fail_next_apply(&self, fail: bool) {
        *self.fail_next_apply.lock().unwrap() = fail;
    }

    fn set_fail_next_set_mode(&self, fail: bool) {
        *self.fail_next_set_mode.lock().unwrap() = fail;
    }

    fn set_console_session(&self, sid: Option<u32>) {
        *self.session_id.lock().unwrap() = sid;
    }

    fn set_has_active_physical(&self, has: bool) {
        *self.has_active_physical.lock().unwrap() = has;
    }

    fn set_restore_notifier(&self, tx: mpsc::Sender<RestoreCcdReport>) {
        *self.restore_notify.lock().unwrap() = Some(tx);
    }

    fn ops(&self) -> Vec<LauncherOp> {
        self.ops.lock().unwrap().clone()
    }
}

fn physical_snapshot_fixture(stamp: u64) -> TopologySnapshot {
    TopologySnapshot {
        schema_version: TOPOLOGY_SNAPSHOT_SCHEMA_VERSION,
        created_at_unix_ms: stamp,
        paths: vec![DisplayPathRecord {
            adapter_luid: AdapterLuid::new(0x1111, 0),
            target_id: 1,
            source_id: 0,
            gdi_name: r"\\.\DISPLAY1".to_string(),
            monitor_friendly_name: "Physical Primary".to_string(),
            active: true,
            target_available: true,
            width: 2560,
            height: 1440,
            origin_x: 0,
            origin_y: 0,
            refresh_num: 144,
            refresh_den: 1,
            scaling: DisplayScaling::Identity,
            rotation: DisplayRotation::Identity,
            scanline_ordering: DisplayScanlineOrdering::Progressive,
            hdr_state: None,
        }],
        hdr_states: Vec::new(),
    }
}

impl ConsoleCcdRunner for FakeLauncher {
    fn console_session_id(&self) -> Option<u32> {
        *self.session_id.lock().unwrap()
    }

    fn snapshot(&self, snapshot_path: &Path, force_overwrite: bool) -> Result<(), LauncherError> {
        if self.console_session_id().is_none() {
            return Err(LauncherError::ConsoleSessionUnavailable);
        }
        {
            let mut fail = self.fail_next_snapshot.lock().unwrap();
            if *fail {
                *fail = false;
                return Err(LauncherError::CommandFailed {
                    exit_code: 1,
                    output: "simulated snapshot failure".to_string(),
                });
            }
        }
        self.ops
            .lock()
            .unwrap()
            .push(LauncherOp::Snapshot(snapshot_path.to_path_buf()));
        if !force_overwrite && snapshot_path.exists() {
            return Ok(());
        }
        let mut stamp = self.snapshot_counter.lock().unwrap();
        let snap = physical_snapshot_fixture(*stamp);
        *stamp += 1;
        save_snapshot_atomic(snapshot_path, &snap)?;
        Ok(())
    }

    fn apply(&self, payload: &ApplyCcdPayload) -> Result<Option<DisplayPathRecord>, LauncherError> {
        if self.console_session_id().is_none() {
            return Err(LauncherError::ConsoleSessionUnavailable);
        }
        {
            let mut fail = self.fail_next_apply.lock().unwrap();
            if *fail {
                *fail = false;
                return Err(LauncherError::CommandFailed {
                    exit_code: 1,
                    output: "simulated apply failure".to_string(),
                });
            }
        }
        self.ops
            .lock()
            .unwrap()
            .push(LauncherOp::Apply(payload.clone()));
        Ok(Some(DisplayPathRecord {
            adapter_luid: AdapterLuid::new(payload.adapter_luid_low, payload.adapter_luid_high),
            target_id: payload.target_id,
            source_id: 1,
            gdi_name: r"\\.\DISPLAY2".to_string(),
            monitor_friendly_name: "ZeroVdd Virtual Display".to_string(),
            active: true,
            target_available: true,
            width: payload.width,
            height: payload.height,
            origin_x: 0,
            origin_y: 0,
            refresh_num: payload.refresh_num,
            refresh_den: payload.refresh_den,
            scaling: DisplayScaling::Identity,
            rotation: DisplayRotation::Identity,
            scanline_ordering: DisplayScanlineOrdering::Progressive,
            hdr_state: None,
        }))
    }

    fn set_mode(
        &self,
        payload: &SetModeCcdPayload,
    ) -> Result<Option<DisplayPathRecord>, LauncherError> {
        if self.console_session_id().is_none() {
            return Err(LauncherError::ConsoleSessionUnavailable);
        }
        {
            let mut fail = self.fail_next_set_mode.lock().unwrap();
            if *fail {
                *fail = false;
                return Err(LauncherError::CommandFailed {
                    exit_code: 1,
                    output: "simulated set-mode CCD failure".to_string(),
                });
            }
        }
        self.ops
            .lock()
            .unwrap()
            .push(LauncherOp::SetMode(payload.clone()));
        Ok(Some(DisplayPathRecord {
            adapter_luid: AdapterLuid::new(payload.adapter_luid_low, payload.adapter_luid_high),
            target_id: payload.target_id,
            source_id: 1,
            gdi_name: r"\\.\DISPLAY2".to_string(),
            monitor_friendly_name: "ZeroVdd Virtual Display".to_string(),
            active: true,
            target_available: true,
            width: payload.width,
            height: payload.height,
            origin_x: 0,
            origin_y: 0,
            refresh_num: payload.refresh_num,
            refresh_den: payload.refresh_den,
            scaling: DisplayScaling::Identity,
            rotation: DisplayRotation::Identity,
            scanline_ordering: DisplayScanlineOrdering::Progressive,
            hdr_state: None,
        }))
    }

    fn restore(
        &self,
        snapshot_path: &Path,
        virtual_target: Option<(AdapterLuid, u32)>,
    ) -> Result<RestoreCcdReport, LauncherError> {
        if self.console_session_id().is_none() {
            return Err(LauncherError::ConsoleSessionUnavailable);
        }
        {
            let mut fail = self.fail_next_restore.lock().unwrap();
            if *fail {
                *fail = false;
                return Err(LauncherError::CommandFailed {
                    exit_code: 1,
                    output: "simulated transient restore failure".to_string(),
                });
            }
        }
        self.ops.lock().unwrap().push(LauncherOp::Restore(
            snapshot_path.to_path_buf(),
            virtual_target,
        ));
        let found = snapshot_path.exists();
        if found {
            let _ = fs::remove_file(snapshot_path);
        }
        let report = RestoreCcdReport {
            snapshot_found: found,
            restored_path_count: usize::from(found),
            skipped_unplugged_targets: 0,
        };
        if let Some(ref tx) = *self.restore_notify.lock().unwrap() {
            let _ = tx.send(report.clone());
        }
        Ok(report)
    }

    fn probe_has_active_displays(&self) -> Result<bool, LauncherError> {
        if self.console_session_id().is_none() {
            return Err(LauncherError::ConsoleSessionUnavailable);
        }
        Ok(*self.has_active_physical.lock().unwrap())
    }
}

struct WatchedEntry {
    pid: u32,
    expected_creation: u64,
    canceled: Arc<AtomicBool>,
    on_exit: Option<Box<dyn FnOnce() + Send>>,
}

struct FakeWatchHandle {
    canceled: Arc<AtomicBool>,
}

impl WatchHandle for FakeWatchHandle {
    fn cancel(&self) {
        self.canceled.store(true, Ordering::Release);
    }
}

struct FakeProcessWatcher {
    entries: Mutex<VecDeque<WatchedEntry>>,
    reject_creation_mismatch: Mutex<BTreeMap<u32, u64>>,
}

impl FakeProcessWatcher {
    fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            reject_creation_mismatch: Mutex::new(BTreeMap::new()),
        }
    }

    fn set_actual_creation_time(&self, pid: u32, actual: u64) {
        self.reject_creation_mismatch
            .lock()
            .unwrap()
            .insert(pid, actual);
    }

    fn watched_creation_time(&self, pid: u32) -> Option<u64> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.pid == pid && !e.canceled.load(Ordering::Acquire))
            .map(|e| e.expected_creation)
    }

    fn trigger_exit(&self, pid: u32) -> bool {
        let callback = {
            let mut guard = self.entries.lock().unwrap();
            let pos = guard
                .iter()
                .position(|e| e.pid == pid && !e.canceled.load(Ordering::Acquire));
            pos.and_then(|idx| guard.remove(idx)).and_then(|mut entry| {
                if entry.canceled.load(Ordering::Acquire) {
                    None
                } else {
                    entry.on_exit.take()
                }
            })
        };
        if let Some(cb) = callback {
            cb();
            true
        } else {
            false
        }
    }
}

impl ProcessWatcher for FakeProcessWatcher {
    fn watch_process(
        &self,
        pid: u32,
        expected_creation_time: u64,
        on_exit: Box<dyn FnOnce() + Send>,
    ) -> Result<Box<dyn WatchHandle>, SessionError> {
        if let Some(&actual) = self.reject_creation_mismatch.lock().unwrap().get(&pid) {
            if expected_creation_time != 0 && actual != expected_creation_time {
                return Err(SessionError::OwnerProcessCreationMismatch {
                    pid,
                    expected: expected_creation_time,
                    actual,
                });
            }
        }
        let canceled = Arc::new(AtomicBool::new(false));
        self.entries.lock().unwrap().push_back(WatchedEntry {
            pid,
            expected_creation: expected_creation_time,
            canceled: Arc::clone(&canceled),
            on_exit: Some(on_exit),
        });
        Ok(Box::new(FakeWatchHandle { canceled }))
    }
}

struct DuplexBuffer {
    reader: Cursor<Vec<u8>>,
    written: Vec<u8>,
}

impl DuplexBuffer {
    fn new(input: Vec<u8>) -> Self {
        Self {
            reader: Cursor::new(input),
            written: Vec::new(),
        }
    }
}

impl Read for DuplexBuffer {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Write for DuplexBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.written.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn roundtrip_ipc(coord: &Arc<ServiceCoordinator>, request_id: u64, req: IpcRequest) -> IpcResponse {
    let raw = encode_frame(&IpcEnvelope::new(request_id, req)).unwrap();
    let mut stream = DuplexBuffer::new(raw);
    handle_pipe_stream(&mut stream, coord.as_ref()).unwrap();
    let reply: IpcEnvelope<IpcResponse> = decode_frame(&stream.written).unwrap();
    assert_eq!(reply.version, IPC_PROTOCOL_VERSION);
    assert_eq!(reply.request_id, request_id);
    reply.payload
}

#[test]
fn normal_start_and_stop_session_snapshots_adds_applies_and_restores() {
    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(snapshot_path.clone()),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );
    coord.initialize_on_startup().unwrap();

    let status = coord
        .start_session(&StartSessionRequest {
            client_id: "moonlight-deck".to_string(),
            width: 2560,
            height: 1600,
            refresh_mhz: 90_000,
            hdr: true,
            owner_pid: 4100,
            owner_creation_time: 123_456_789,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap();

    assert!(status.active);
    assert_eq!(status.client_id.as_deref(), Some("moonlight-deck"));
    assert_eq!(status.display_name.as_deref(), Some(r"\\.\DISPLAY2"));
    assert_eq!(status.width, 2560);
    assert_eq!(status.height, 1600);
    assert_eq!(status.refresh_mhz, 90_000);
    assert!(status.hdr_active);
    assert_eq!(watcher.watched_creation_time(4100), Some(123_456_789));
    assert_eq!(driver.active_count(), 1);
    assert!(snapshot_path.exists());

    let restored = coord
        .stop_session(&StopSessionRequest {
            client_id: Some("moonlight-deck".to_string()),
            owner_pid: Some(4100),
            force: false,
        })
        .unwrap();
    assert!(restored);
    assert!(!coord.get_session_status().active);
    assert_eq!(driver.active_count(), 0);
    assert!(!snapshot_path.exists());

    let ops = launcher.ops();
    assert_eq!(ops.len(), 3);
    assert!(matches!(&ops[0], LauncherOp::Snapshot(p) if p == &snapshot_path));
    assert!(matches!(
        &ops[1],
        LauncherOp::Apply(payload) if payload.snapshot_path.is_none()
            && payload.width == 2560
            && payload.height == 1600
            && payload.refresh_num == 90
            && payload.refresh_den == 1
            && payload.hdr
    ));
    assert!(matches!(&ops[2], LauncherOp::Restore(p, Some(_)) if p == &snapshot_path));
}

#[test]
fn owner_pid_exit_automatically_departs_virtual_display_and_restores_snapshot() {
    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let (restore_tx, restore_rx) = mpsc::channel();
    launcher.set_restore_notifier(restore_tx);

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(snapshot_path.clone()),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    coord
        .start_session(&StartSessionRequest {
            client_id: "sunshine-client".to_string(),
            width: 3840,
            height: 2160,
            refresh_mhz: 119_880,
            hdr: true,
            owner_pid: 7777,
            owner_creation_time: 99_999,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap();
    assert_eq!(driver.active_count(), 1);
    assert!(snapshot_path.exists());

    let barrier = Arc::new(Barrier::new(2));
    let barrier_bg = Arc::clone(&barrier);
    let watcher_bg = Arc::clone(&watcher);
    let handle = thread::spawn(move || {
        barrier_bg.wait();
        assert!(watcher_bg.trigger_exit(7777));
    });

    barrier.wait();
    let report = restore_rx.recv().unwrap();
    handle.join().unwrap();

    assert!(report.snapshot_found);
    assert_eq!(driver.active_count(), 0);
    assert!(!coord.get_session_status().active);
    assert!(!snapshot_path.exists());
}

#[test]
fn pid_reuse_creation_time_mismatch_rejects_session_and_rolls_back() {
    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());
    watcher.set_actual_creation_time(5555, 2222);

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(snapshot_path.clone()),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    let err = coord
        .start_session(&StartSessionRequest {
            client_id: "reused-pid".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            owner_pid: 5555,
            owner_creation_time: 1111,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap_err();

    assert!(matches!(
        err,
        SessionError::OwnerProcessCreationMismatch {
            pid: 5555,
            expected: 1111,
            actual: 2222
        }
    ));
    assert_eq!(driver.active_count(), 0);
    assert!(!coord.get_session_status().active);
    assert!(!snapshot_path.exists());
}

#[test]
fn overlapping_second_start_session_preserves_original_topology_backup() {
    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(snapshot_path.clone()),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    coord
        .start_session(&StartSessionRequest {
            client_id: "client-one".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            owner_pid: 1001,
            owner_creation_time: 10_001,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap();

    let original_backup_bytes = fs::read(&snapshot_path).unwrap();
    assert_eq!(
        driver.active_guids(),
        vec![monitor_guid_for_name("client-one")]
    );

    let second = coord
        .start_session(&StartSessionRequest {
            client_id: "client-two".to_string(),
            width: 3840,
            height: 2160,
            refresh_mhz: 119_880,
            hdr: true,
            owner_pid: 2002,
            owner_creation_time: 20_002,
            topology_mode: TopologyMode::Extend,
        })
        .unwrap();

    assert_eq!(second.client_id.as_deref(), Some("client-two"));
    assert_eq!(second.width, 3840);
    assert_eq!(second.height, 2160);
    assert_eq!(
        driver.active_guids(),
        vec![monitor_guid_for_name("client-two")]
    );
    assert_eq!(fs::read(&snapshot_path).unwrap(), original_backup_bytes);

    // WHY: Triggering the canceled first session's watcher must be a clean no-op.
    assert!(!watcher.trigger_exit(1001));
    assert!(coord.get_session_status().active);

    let snapshot_count = launcher
        .ops()
        .iter()
        .filter(|op| matches!(op, LauncherOp::Snapshot(_)))
        .count();
    assert_eq!(snapshot_count, 1);

    let restored = coord
        .stop_session(&StopSessionRequest {
            client_id: None,
            owner_pid: None,
            force: false,
        })
        .unwrap();
    assert!(restored);
    assert_eq!(driver.active_count(), 0);
    assert!(!snapshot_path.exists());
}

#[test]
fn startup_leftover_backup_defers_when_no_console_and_restores_on_session_logon() {
    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    save_snapshot_atomic(&snapshot_path, &physical_snapshot_fixture(42)).unwrap();

    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(None));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(snapshot_path.clone()),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    coord.initialize_on_startup().unwrap();
    assert!(coord.is_pending_startup_restore());
    assert!(snapshot_path.exists());
    assert!(launcher.ops().is_empty());

    assert!(!coord.on_session_change(ConsoleSessionEvent::Other).unwrap());
    assert!(coord.is_pending_startup_restore());

    launcher.set_console_session(Some(1));
    assert!(coord
        .on_session_change(ConsoleSessionEvent::SessionLogon)
        .unwrap());
    assert!(!coord.is_pending_startup_restore());
    assert!(!snapshot_path.exists());
    assert_eq!(
        launcher.ops(),
        vec![LauncherOp::Restore(snapshot_path.clone(), None)]
    );

    // WHY: Also verify immediate startup restore when a console session is already active.
    save_snapshot_atomic(&snapshot_path, &physical_snapshot_fixture(99)).unwrap();
    let launcher_ready = Arc::new(FakeLauncher::new(Some(1)));
    let coord_ready = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(snapshot_path.clone()),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher_ready) as _,
        Arc::clone(&watcher) as _,
    );
    coord_ready.initialize_on_startup().unwrap();
    assert!(!coord_ready.is_pending_startup_restore());
    assert!(!snapshot_path.exists());
    assert_eq!(
        launcher_ready.ops(),
        vec![LauncherOp::Restore(snapshot_path, None)]
    );
}

#[test]
fn ipc_dispatch_and_persistent_headless_standby_profiles_work_end_to_end() {
    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    launcher.set_has_active_physical(false);
    let watcher = Arc::new(FakeProcessWatcher::new());

    let config = ConfigV1 {
        headless_standby: true,
        persistent_displays: vec![PersistentDisplayConfig {
            name: "work-monitor".to_string(),
            width: 1920,
            height: 1200,
            refresh_mhz: 60_000,
            hdr: false,
            ..Default::default()
        }],
        ..Default::default()
    };

    let coord = ServiceCoordinator::new(
        config,
        Some(snapshot_path.clone()),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );
    coord.initialize_on_startup().unwrap();

    assert!(coord.is_headless_standby_active());
    assert_eq!(
        coord.persistent_guids(),
        vec![monitor_guid_for_name("work-monitor")]
    );
    assert!(driver
        .active_guids()
        .contains(&monitor_guid_for_name(HEADLESS_STANDBY_CLIENT_ID)));

    let start_reply = roundtrip_ipc(
        &coord,
        1,
        IpcRequest::StartSession(StartSessionRequest {
            client_id: "ipad-pro".to_string(),
            width: 2388,
            height: 1668,
            refresh_mhz: 120_000,
            hdr: true,
            owner_pid: 8888,
            owner_creation_time: 88_888,
            topology_mode: TopologyMode::Isolate,
        }),
    );
    assert!(matches!(start_reply, IpcResponse::SessionStarted(ref s) if s.active));
    assert!(!coord.is_headless_standby_active());

    let status_reply = roundtrip_ipc(&coord, 2, IpcRequest::GetSessionStatus);
    assert!(matches!(
        status_reply,
        IpcResponse::SessionStatus(ref s) if s.client_id.as_deref() == Some("ipad-pro")
    ));

    let add_reply = roundtrip_ipc(
        &coord,
        3,
        IpcRequest::DisplayAdd(DisplayAddRequest {
            client_id: "extra-sidecar".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            lease_ttl_secs: None,
            topology_mode: None,
            ..Default::default()
        }),
    );
    let added_target = match add_reply {
        IpcResponse::DisplayAdded { target_id, .. } => target_id,
        other => panic!("expected DisplayAdded, got {other:?}"),
    };

    let health_reply = roundtrip_ipc(&coord, 4, IpcRequest::GetHealth);
    assert!(matches!(
        health_reply,
        IpcResponse::Health(ref h) if h.driver_connected && h.backup_present && h.active_displays >= 2
    ));

    let remove_reply = roundtrip_ipc(
        &coord,
        5,
        IpcRequest::DisplayRemove(DisplayRemoveRequest {
            client_id: None,
            target_id: Some(added_target),
        }),
    );
    assert_eq!(remove_reply, IpcResponse::DisplayRemoved { removed: true });

    let bad_stop = roundtrip_ipc(
        &coord,
        6,
        IpcRequest::StopSession(StopSessionRequest {
            client_id: Some("wrong-client".to_string()),
            owner_pid: None,
            force: false,
        }),
    );
    assert!(matches!(bad_stop, IpcResponse::Error(ref e) if e.code == "session_owner_mismatch"));

    let stop_reply = roundtrip_ipc(
        &coord,
        7,
        IpcRequest::StopSession(StopSessionRequest {
            client_id: Some("ipad-pro".to_string()),
            owner_pid: Some(8888),
            force: false,
        }),
    );
    assert_eq!(
        stop_reply,
        IpcResponse::SessionStopped {
            restored_topology: true
        }
    );
    assert!(coord.is_headless_standby_active());

    let bad_version_raw = encode_frame(&IpcEnvelope {
        version: 99,
        request_id: 8,
        payload: IpcRequest::GetHealth,
    })
    .unwrap();
    let mut bad_stream = DuplexBuffer::new(bad_version_raw);
    handle_pipe_stream(&mut bad_stream, coord.as_ref()).unwrap();
    let bad_reply: IpcEnvelope<IpcResponse> = decode_frame(&bad_stream.written).unwrap();
    assert!(matches!(
        bad_reply.payload,
        IpcResponse::Error(ref e) if e.code == "unsupported_protocol_version"
    ));

    assert!(IPC_PIPE_SDDL.contains("(A;;GA;;;SY)"));
    assert!(IPC_PIPE_SDDL.contains("(A;;GA;;;BA)"));
    assert!(IPC_PIPE_SDDL.contains("(A;;GRGW;;;IU)"));
    assert!(!IPC_PIPE_SDDL.contains("WD"));

    let exe_dir = TempDir::new().unwrap();
    let svc_exe = exe_dir.path().join("zerovdd-svc.exe");
    let cli_name = if cfg!(windows) {
        "zerovdd.exe"
    } else {
        "zerovdd"
    };
    let cli_exe = exe_dir.path().join(cli_name);
    fs::write(&cli_exe, b"stub").unwrap();
    assert_eq!(resolve_sibling_cli_path(&svc_exe).unwrap(), cli_exe);
}

struct TrackingDriverProvider {
    client: Arc<dyn ControlClient + Send + Sync>,
    reset_count: AtomicUsize,
}

impl TrackingDriverProvider {
    fn new(client: Arc<dyn ControlClient + Send + Sync>) -> Self {
        Self {
            client,
            reset_count: AtomicUsize::new(0),
        }
    }

    fn resets(&self) -> usize {
        self.reset_count.load(Ordering::Acquire)
    }
}

impl DriverProvider for TrackingDriverProvider {
    fn get_or_connect(&self) -> Result<Arc<dyn ControlClient + Send + Sync>, SessionError> {
        Ok(Arc::clone(&self.client))
    }

    fn connected_client(&self) -> Option<Arc<dyn ControlClient + Send + Sync>> {
        Some(Arc::clone(&self.client))
    }

    fn reset(&self) {
        self.reset_count.fetch_add(1, Ordering::AcqRel);
    }
}

struct NonBlockingSignalWatchHandle {
    cancel_tx: Mutex<Option<mpsc::Sender<()>>>,
}

impl WatchHandle for NonBlockingSignalWatchHandle {
    fn cancel(&self) {
        if let Some(tx) = self.cancel_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
}

struct ConcurrentRaceWatcher {
    worker_done_rx: Mutex<Option<mpsc::Receiver<bool>>>,
    barrier: Arc<Barrier>,
}

impl ConcurrentRaceWatcher {
    fn new(barrier: Arc<Barrier>) -> Self {
        Self {
            worker_done_rx: Mutex::new(None),
            barrier,
        }
    }

    fn take_worker_done_rx(&self) -> mpsc::Receiver<bool> {
        self.worker_done_rx.lock().unwrap().take().unwrap()
    }
}

impl ProcessWatcher for ConcurrentRaceWatcher {
    fn watch_process(
        &self,
        _pid: u32,
        _expected_creation_time: u64,
        on_exit: Box<dyn FnOnce() + Send>,
    ) -> Result<Box<dyn WatchHandle>, SessionError> {
        let (cancel_tx, cancel_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<bool>();
        *self.worker_done_rx.lock().unwrap() = Some(done_rx);
        let barrier = Arc::clone(&self.barrier);

        thread::spawn(move || {
            let _ = cancel_rx.recv();
            barrier.wait();
            on_exit();
            let _ = done_tx.send(true);
        });

        Ok(Box::new(NonBlockingSignalWatchHandle {
            cancel_tx: Mutex::new(Some(cancel_tx)),
        }))
    }
}

#[test]
fn concurrent_stop_session_and_owner_exit_cannot_deadlock() {
    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let barrier = Arc::new(Barrier::new(2));
    let watcher = Arc::new(ConcurrentRaceWatcher::new(Arc::clone(&barrier)));

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(snapshot_path.clone()),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    coord
        .start_session(&StartSessionRequest {
            client_id: "race-client".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            owner_pid: 4242,
            owner_creation_time: 100,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap();

    let done_rx = watcher.take_worker_done_rx();
    let coord_stop = Arc::clone(&coord);
    let stop_handle = thread::spawn(move || {
        // WHY: `stop_session` acquires `op_mutex`, calls `WatchHandle::cancel()`, which wakes the
        // watcher thread to enter `handle_owner_exit` concurrently via `barrier`.
        let res = coord_stop.stop_session(&StopSessionRequest {
            client_id: None,
            owner_pid: None,
            force: true,
        });
        res.unwrap()
    });

    barrier.wait();
    let stop_restored = stop_handle.join().unwrap();
    assert!(done_rx.recv().unwrap());
    assert!(stop_restored);
    assert_eq!(driver.active_count(), 0);
    assert!(!coord.get_session_status().active);
    assert!(!snapshot_path.exists());

    let restore_count = launcher
        .ops()
        .iter()
        .filter(|op| matches!(op, LauncherOp::Restore(_, _)))
        .count();
    assert_eq!(restore_count, 1);
}

#[test]
fn overlapping_start_session_restores_backup_when_add_display_fails() {
    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    let driver = Arc::new(FakeDriver::new());
    let provider = Arc::new(TrackingDriverProvider::new(Arc::clone(&driver) as _));
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(snapshot_path.clone()),
        Arc::clone(&provider) as _,
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    coord
        .start_session(&StartSessionRequest {
            client_id: "first-client".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            owner_pid: 3001,
            owner_creation_time: 10_001,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap();
    assert!(snapshot_path.exists());

    driver.set_fail_next_add(true);
    let err = coord
        .start_session(&StartSessionRequest {
            client_id: "second-client".to_string(),
            width: 2560,
            height: 1440,
            refresh_mhz: 120_000,
            hdr: true,
            owner_pid: 3002,
            owner_creation_time: 10_002,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap_err();

    assert!(matches!(
        err,
        SessionError::Client(ClientError::DeviceNotFound(_))
    ));
    assert_eq!(provider.resets(), 1);
    assert_eq!(driver.active_count(), 0);
    assert!(!coord.get_session_status().active);
    assert!(!snapshot_path.exists());
    assert!(launcher
        .ops()
        .iter()
        .any(|op| matches!(op, LauncherOp::Restore(p, None) if p == &snapshot_path)));
}

#[test]
fn transient_restore_error_keeps_pending_startup_restore_until_session_change() {
    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    save_snapshot_atomic(&snapshot_path, &physical_snapshot_fixture(77)).unwrap();

    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    launcher.set_has_active_physical(false);
    launcher.set_fail_next_restore(true);
    let watcher = Arc::new(FakeProcessWatcher::new());

    let config = ConfigV1 {
        headless_standby: true,
        ..Default::default()
    };

    let coord = ServiceCoordinator::new(
        config,
        Some(snapshot_path.clone()),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    let err = coord.initialize_on_startup().unwrap_err();
    assert!(matches!(err, SessionError::Launcher(_)));
    assert!(coord.is_pending_startup_restore());
    assert!(!coord.is_headless_standby_active());
    assert!(snapshot_path.exists());

    assert!(coord
        .on_session_change(ConsoleSessionEvent::SessionLogon)
        .unwrap());
    assert!(!coord.is_pending_startup_restore());
    assert!(!snapshot_path.exists());
    assert!(coord.is_headless_standby_active());
}

#[test]
fn display_remove_by_target_id_clears_headless_standby_tracking() {
    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    launcher.set_has_active_physical(false);
    let watcher = Arc::new(FakeProcessWatcher::new());

    let config = ConfigV1 {
        headless_standby: true,
        ..Default::default()
    };

    let coord = ServiceCoordinator::new(
        config,
        Some(snapshot_path),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );
    coord.initialize_on_startup().unwrap();
    assert!(coord.is_headless_standby_active());

    let list = driver.list_displays().unwrap();
    let standby_target = list.entries().unwrap()[0].target_id;
    let removed = coord
        .display_remove(&DisplayRemoveRequest {
            client_id: None,
            target_id: Some(standby_target),
        })
        .unwrap();
    assert!(removed);
    assert!(!coord.is_headless_standby_active());
}

#[test]
fn domain_win32_ioctl_error_preserves_cached_driver_handle() {
    let dir = TempDir::new().unwrap();
    let driver = Arc::new(FakeDriver::new());
    let provider = Arc::new(TrackingDriverProvider::new(Arc::clone(&driver) as _));
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(dir.path().join("topology-backup.json")),
        Arc::clone(&provider) as _,
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    let add_ok = coord
        .display_add(&DisplayAddRequest {
            client_id: "persistent-1".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            lease_ttl_secs: None,
            topology_mode: None,
            ..Default::default()
        })
        .unwrap();
    assert!(matches!(add_ok, IpcResponse::DisplayAdded { .. }));

    driver.set_fail_next_add_err(ClientError::Win32 {
        operation: "DeviceIoControl(IOCTL_ADD_DISPLAY)",
        code: 0x8007_00B7,
    });
    let err = coord
        .display_add(&DisplayAddRequest {
            client_id: "conflict".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            lease_ttl_secs: None,
            topology_mode: None,
            ..Default::default()
        })
        .unwrap_err();
    assert!(matches!(
        err,
        SessionError::Client(ClientError::Win32 { .. })
    ));
    assert_eq!(provider.resets(), 0);
    assert_eq!(driver.active_count(), 1);
}

#[test]
fn stale_handle_win32_error_resets_provider_when_version_probe_fails() {
    let dir = TempDir::new().unwrap();
    let driver = Arc::new(FakeDriver::new());
    let provider = Arc::new(TrackingDriverProvider::new(Arc::clone(&driver) as _));
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(dir.path().join("topology-backup.json")),
        Arc::clone(&provider) as _,
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    driver.set_fail_get_version(true);
    driver.set_fail_next_add_err(ClientError::Win32 {
        operation: "DeviceIoControl(IOCTL_ADD_DISPLAY)",
        code: 0x8007_0006,
    });
    let err = coord
        .display_add(&DisplayAddRequest {
            client_id: "broken-handle".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            lease_ttl_secs: None,
            topology_mode: None,
            ..Default::default()
        })
        .unwrap_err();
    assert!(matches!(
        err,
        SessionError::Client(ClientError::Win32 { .. })
    ));
    assert_eq!(provider.resets(), 1);
}

#[test]
fn overlapping_start_session_when_prev_display_already_departed_preserves_handle() {
    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    let driver = Arc::new(FakeDriver::new());
    let provider = Arc::new(TrackingDriverProvider::new(Arc::clone(&driver) as _));
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(snapshot_path),
        Arc::clone(&provider) as _,
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    coord
        .start_session(&StartSessionRequest {
            client_id: "first-client".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            owner_pid: 4001,
            owner_creation_time: 10_001,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap();

    let first_guid = monitor_guid_for_name("first-client");
    driver
        .remove_display(&RemoveDisplayRequest::by_guid(first_guid))
        .unwrap();
    assert_eq!(driver.active_count(), 0);

    let status = coord
        .start_session(&StartSessionRequest {
            client_id: "second-client".to_string(),
            width: 2560,
            height: 1440,
            refresh_mhz: 120_000,
            hdr: true,
            owner_pid: 4002,
            owner_creation_time: 10_002,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap();
    assert!(status.active);
    assert_eq!(provider.resets(), 0);
    assert_eq!(driver.active_count(), 1);
}

#[test]
fn start_session_snapshot_failure_reconciles_headless_standby() {
    let dir = TempDir::new().unwrap();
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    launcher.set_has_active_physical(false);
    let watcher = Arc::new(FakeProcessWatcher::new());

    let config = ConfigV1 {
        headless_standby: true,
        ..Default::default()
    };

    let coord = ServiceCoordinator::new(
        config,
        Some(dir.path().join("topology-backup.json")),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );
    coord.initialize_on_startup().unwrap();
    assert!(coord.is_headless_standby_active());
    assert_eq!(driver.active_count(), 1);

    launcher.set_fail_next_snapshot(true);
    let err = coord
        .start_session(&StartSessionRequest {
            client_id: "stream-client".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            owner_pid: 5001,
            owner_creation_time: 10_005,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap_err();
    assert!(matches!(err, SessionError::Launcher(_)));
    assert!(coord.is_headless_standby_active());
    assert_eq!(driver.active_count(), 1);
}

#[test]
fn display_set_mode_updates_driver_ccd_and_active_session_status() {
    let dir = TempDir::new().unwrap();
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = Arc::new(ServiceCoordinator::new(
        ConfigV1::default(),
        Some(dir.path().join("topology-backup.json")),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    ));
    coord.initialize_on_startup().unwrap();

    let start_status = coord
        .start_session(&StartSessionRequest {
            client_id: "stream-client".to_string(),
            width: 2560,
            height: 1440,
            refresh_mhz: 60_000,
            hdr: false,
            owner_pid: 6001,
            owner_creation_time: 10_006,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap();
    assert!(start_status.active);
    let target_id = driver.state.lock().unwrap().displays[0].target_id;

    let update_resp = roundtrip_ipc(
        &coord,
        1,
        IpcRequest::DisplaySetMode(DisplaySetModeRequest {
            client_id: Some("stream-client".to_string()),
            target_id: None,
            width: 3024,
            height: 1890,
            refresh_mhz: 120_000,
            hdr: Some(true),
            ..Default::default()
        }),
    );
    assert!(matches!(
        update_resp,
        IpcResponse::DisplayModeUpdated {
            target_id: t,
            width: 3024,
            height: 1890,
            refresh_mhz: 120_000,
            ..
        } if t == target_id
    ));

    let driver_guard = driver.state.lock().unwrap();
    assert_eq!(driver_guard.update_mode_calls.len(), 1);
    assert_eq!(driver_guard.displays[0].req.width, 3024);
    assert_eq!(driver_guard.displays[0].req.height, 1890);
    assert_eq!(driver_guard.displays[0].req.refresh_num, 120);
    assert_eq!(driver_guard.displays[0].req.refresh_den, 1);
    assert_eq!(driver_guard.displays[0].req.hdr_profile, 1);
    drop(driver_guard);

    let ops = launcher.ops();
    assert!(ops.iter().any(|op| matches!(
        op,
        LauncherOp::SetMode(payload)
            if payload.target_id == target_id
                && payload.width == 3024
                && payload.height == 1890
                && payload.refresh_num == 120
                && payload.refresh_den == 1
                && payload.hdr == Some(true)
    )));

    match roundtrip_ipc(&coord, 2, IpcRequest::GetSessionStatus) {
        IpcResponse::SessionStatus(status) => {
            assert!(status.active);
            assert_eq!(status.width, 3024);
            assert_eq!(status.height, 1890);
            assert_eq!(status.refresh_mhz, 120_000);
            assert!(status.hdr_active);
        }
        other => panic!("unexpected status response: {other:?}"),
    }

    let _ = coord
        .stop_session(&StopSessionRequest {
            client_id: Some("stream-client".to_string()),
            owner_pid: None,
            force: false,
        })
        .unwrap();
}

#[test]
fn display_set_mode_requires_console_and_rolls_back_driver_on_ccd_failure() {
    let dir = TempDir::new().unwrap();
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = Arc::new(ServiceCoordinator::new(
        ConfigV1::default(),
        Some(dir.path().join("topology-backup.json")),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    ));
    coord.initialize_on_startup().unwrap();

    coord
        .start_session(&StartSessionRequest {
            client_id: "stream-client".to_string(),
            width: 2560,
            height: 1440,
            refresh_mhz: 60_000,
            hdr: false,
            owner_pid: 6002,
            owner_creation_time: 10_007,
            topology_mode: TopologyMode::Isolate,
        })
        .unwrap();

    launcher.set_console_session(None);
    let no_console_err = coord
        .display_set_mode(&DisplaySetModeRequest {
            client_id: Some("stream-client".to_string()),
            target_id: None,
            width: 3024,
            height: 1890,
            refresh_mhz: 120_000,
            hdr: Some(true),
            ..Default::default()
        })
        .unwrap_err();
    assert!(matches!(
        no_console_err,
        SessionError::Launcher(LauncherError::ConsoleSessionUnavailable)
    ));
    assert_eq!(driver.state.lock().unwrap().update_mode_calls.len(), 0);

    launcher.set_console_session(Some(1));
    launcher.set_fail_next_set_mode(true);
    let ccd_err = coord
        .display_set_mode(&DisplaySetModeRequest {
            client_id: Some("stream-client".to_string()),
            target_id: None,
            width: 3024,
            height: 1890,
            refresh_mhz: 120_000,
            hdr: Some(true),
            ..Default::default()
        })
        .unwrap_err();
    assert!(matches!(
        ccd_err,
        SessionError::Launcher(LauncherError::CommandFailed { .. })
    ));

    let driver_guard = driver.state.lock().unwrap();
    assert_eq!(driver_guard.update_mode_calls.len(), 2);
    assert_eq!(driver_guard.displays[0].req.width, 2560);
    assert_eq!(driver_guard.displays[0].req.height, 1440);
    assert_eq!(driver_guard.displays[0].req.refresh_num, 60);
    assert_eq!(driver_guard.displays[0].req.refresh_den, 1);
    assert_eq!(driver_guard.displays[0].req.hdr_profile, 0);
    drop(driver_guard);

    let status = coord.get_session_status();
    assert_eq!(status.width, 2560);
    assert_eq!(status.height, 1440);
    assert_eq!(status.refresh_mhz, 60_000);
    assert!(!status.hdr_active);

    let _ = coord
        .stop_session(&StopSessionRequest {
            client_id: Some("stream-client".to_string()),
            owner_pid: None,
            force: false,
        })
        .unwrap();
}

#[test]
fn display_add_with_topology_mode_extend_delegates_to_console_launcher() {
    let dir = TempDir::new().unwrap();
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(dir.path().join("topology-backup.json")),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    let resp = coord
        .display_add(&DisplayAddRequest {
            client_id: "extend-display".to_string(),
            width: 3024,
            height: 1890,
            refresh_mhz: 120_000,
            hdr: true,
            lease_ttl_secs: None,
            topology_mode: Some(TopologyMode::Extend),
            ..Default::default()
        })
        .unwrap();

    let target_id = match resp {
        IpcResponse::DisplayAdded { target_id, .. } => target_id,
        other => panic!("expected DisplayAdded, got {other:?}"),
    };
    assert_eq!(driver.active_count(), 1);
    assert!(launcher.ops().iter().any(|op| matches!(
        op,
        LauncherOp::Apply(payload)
            if payload.target_id == target_id
                && payload.width == 3024
                && payload.height == 1890
                && payload.refresh_num == 120
                && payload.refresh_den == 1
                && payload.hdr
                && payload.topology_mode == SunshineTopologyMode::Extend
                && !payload.make_virtual_primary
                && payload.snapshot_path.is_none()
    )));
}

#[test]
fn display_add_with_topology_mode_isolate_sets_virtual_primary() {
    let dir = TempDir::new().unwrap();
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(dir.path().join("topology-backup.json")),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    let resp = coord
        .display_add(&DisplayAddRequest {
            client_id: "isolate-display".to_string(),
            width: 2560,
            height: 1440,
            refresh_mhz: 60_000,
            hdr: false,
            lease_ttl_secs: None,
            topology_mode: Some(TopologyMode::Isolate),
            ..Default::default()
        })
        .unwrap();

    let target_id = match resp {
        IpcResponse::DisplayAdded { target_id, .. } => target_id,
        other => panic!("expected DisplayAdded, got {other:?}"),
    };
    assert_eq!(driver.active_count(), 1);
    assert!(launcher.ops().iter().any(|op| matches!(
        op,
        LauncherOp::Apply(payload)
            if payload.target_id == target_id
                && payload.topology_mode == SunshineTopologyMode::Isolate
                && payload.make_virtual_primary
                && payload.snapshot_path.is_none()
    )));
}

#[test]
fn display_add_with_topology_mode_rejects_when_console_session_unavailable() {
    let dir = TempDir::new().unwrap();
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(None));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(dir.path().join("topology-backup.json")),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    let err = coord
        .display_add(&DisplayAddRequest {
            client_id: "no-console".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            lease_ttl_secs: None,
            topology_mode: Some(TopologyMode::Extend),
            ..Default::default()
        })
        .unwrap_err();

    assert!(matches!(
        err,
        SessionError::Launcher(LauncherError::ConsoleSessionUnavailable)
    ));
    assert_eq!(driver.active_count(), 0);
    assert!(launcher.ops().is_empty());
}

#[test]
fn display_add_rolls_back_virtual_display_when_launcher_apply_fails() {
    let dir = TempDir::new().unwrap();
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(dir.path().join("topology-backup.json")),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    launcher.set_fail_next_apply(true);
    let err = coord
        .display_add(&DisplayAddRequest {
            client_id: "rollback-display".to_string(),
            width: 3024,
            height: 1890,
            refresh_mhz: 120_000,
            hdr: true,
            lease_ttl_secs: None,
            topology_mode: Some(TopologyMode::Extend),
            ..Default::default()
        })
        .unwrap_err();

    assert!(matches!(
        err,
        SessionError::Launcher(LauncherError::CommandFailed { .. })
    ));
    assert_eq!(driver.active_count(), 0);
}

#[test]
fn display_add_without_topology_mode_succeeds_without_console_session() {
    let dir = TempDir::new().unwrap();
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(None));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let coord = ServiceCoordinator::new(
        ConfigV1::default(),
        Some(dir.path().join("topology-backup.json")),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );

    let resp = coord
        .display_add(&DisplayAddRequest {
            client_id: "headless-no-topo".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            hdr: false,
            lease_ttl_secs: None,
            topology_mode: None,
            ..Default::default()
        })
        .unwrap();

    assert!(matches!(resp, IpcResponse::DisplayAdded { .. }));
    assert_eq!(driver.active_count(), 1);
    assert!(launcher.ops().is_empty());
}

#[test]
fn display_add_and_set_mode_forward_color_bit_depth_ppi_and_hdr_nits() {
    use zerovdd_config::{BitDepthConfig, ColorSpaceConfig};
    use zerovdd_proto::ipc::{BitDepth, ColorSpace};
    use zerovdd_proto::types::{BitDepthPreference, ColorSpacePreference};

    let dir = TempDir::new().unwrap();
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());

    let cfg = ConfigV1 {
        persistent_displays: vec![PersistentDisplayConfig {
            name: "p3-persistent".to_string(),
            width: 3840,
            height: 2160,
            refresh_mhz: 120_000,
            hdr: true,
            color_space: ColorSpaceConfig::DciP3,
            bit_depth: BitDepthConfig::Bpc10,
            target_ppi: Some(140),
            max_luminance_nits: Some(1600),
            max_fall_nits: Some(500),
            min_luminance_ten_thousandths: Some(250),
            ..Default::default()
        }],
        ..Default::default()
    };

    let coord = ServiceCoordinator::new(
        cfg,
        Some(dir.path().join("topology-backup.json")),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );
    coord.initialize_on_startup().unwrap();

    coord
        .display_add(&DisplayAddRequest {
            client_id: "ipc-p3".to_string(),
            width: 2560,
            height: 1440,
            refresh_mhz: 120_000,
            hdr: true,
            target_ppi: Some(150),
            color_space: Some(ColorSpace::DciP3),
            bit_depth: Some(BitDepth::Bpc10),
            max_luminance_nits: Some(1400),
            max_fall_nits: Some(450),
            min_luminance_ten_thousandths: Some(300),
            ..Default::default()
        })
        .unwrap();

    coord
        .display_set_mode(&DisplaySetModeRequest {
            client_id: Some("ipc-p3".to_string()),
            width: 3840,
            height: 2160,
            refresh_mhz: 120_000,
            hdr: Some(true),
            color_space: Some(ColorSpace::Bt2020),
            bit_depth: Some(BitDepth::Bpc12),
            max_luminance_nits: Some(2000),
            ..Default::default()
        })
        .unwrap();

    let state = driver.state.lock().unwrap();
    assert_eq!(state.add_calls[0].target_ppi, 140);
    assert_eq!(
        state.add_calls[0].color_space,
        ColorSpacePreference::DciP3.as_u8()
    );
    assert_eq!(
        state.add_calls[0].bit_depth,
        BitDepthPreference::Bpc10.as_u8()
    );
    assert_eq!(state.add_calls[0].hdr_metadata.max_luminance_nits, 1600);
    assert_eq!(state.add_calls[1].target_ppi, 150);
    assert_eq!(state.add_calls[1].hdr_metadata.max_luminance_nits, 1400);
    assert_eq!(
        state.update_mode_calls[0].color_space,
        ColorSpacePreference::Bt2020.as_u8()
    );
    assert_eq!(
        state.update_mode_calls[0].bit_depth,
        BitDepthPreference::Bpc12.as_u8()
    );
    assert_eq!(
        state.update_mode_calls[0].hdr_metadata.max_luminance_nits,
        2000
    );
}

#[test]
fn custom_edid_ipc_and_persistent_loading_validate_and_skip_corrupt_files() {
    let dir = TempDir::new().unwrap();
    let valid_128_path = dir.path().join("valid128.edid");
    let valid_256_path = dir.path().join("valid256.edid");
    let missing_path = dir.path().join("missing.edid");

    let valid_256 = zerovdd_edid::synthesize_edid(&zerovdd_edid::EdidSpec::new(
        [0x55; 16],
        "CustomEdid256",
        1920,
        1080,
        60_000,
    ))
    .unwrap();
    let valid_128 = zerovdd_edid::synthesize_block0(&zerovdd_edid::Block0Params {
        monitor_guid: [0x44; 16],
        client_name: "CustomEdid128",
        preferred_width: 1920,
        preferred_height: 1080,
        preferred_refresh_millihz: 60_000,
        target_ppi: None,
        color_profile: zerovdd_edid::ColorProfile::SdrBt709,
        bit_depth_bpc: None,
        extension_count: 0,
    })
    .unwrap();
    fs::write(&valid_128_path, valid_128).unwrap();
    fs::write(&valid_256_path, valid_256).unwrap();

    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());
    let cfg = ConfigV1 {
        persistent_displays: vec![
            PersistentDisplayConfig {
                name: "ok-edid".to_string(),
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                edid_path: Some(valid_128_path.display().to_string()),
                ..Default::default()
            },
            PersistentDisplayConfig {
                name: "missing-edid".to_string(),
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                edid_path: Some(missing_path.display().to_string()),
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let coord = ServiceCoordinator::new(
        cfg,
        Some(dir.path().join("topology-backup.json")),
        Arc::clone(
            &(Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _))
                as Arc<dyn DriverProvider>),
        ),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
    );
    coord.initialize_on_startup().unwrap();
    assert_eq!(coord.persistent_guids().len(), 1);

    coord
        .display_add(&DisplayAddRequest {
            client_id: "ipc-edid-256".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            custom_edid: Some(valid_256.to_vec()),
            ..Default::default()
        })
        .unwrap();

    let mut corrupt = valid_128;
    corrupt[127] ^= 0xFF;
    let corrupt_err = coord
        .display_add(&DisplayAddRequest {
            client_id: "ipc-edid-corrupt".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            custom_edid: Some(corrupt.to_vec()),
            ..Default::default()
        })
        .unwrap_err();
    assert!(matches!(corrupt_err, SessionError::InvalidEdid(_)));

    let state = driver.state.lock().unwrap();
    assert_eq!(state.add_calls[0].custom_edid_slice().unwrap(), &valid_128);
    assert_eq!(state.add_calls[1].custom_edid_slice().unwrap(), &valid_256);
}

#[test]
fn unprivileged_ipc_parity_list_gpus_switch_restore_and_layout_add() {
    struct FakeGpuEnumerator {
        adapters: Vec<GpuAdapterInfo>,
    }

    impl GpuEnumerator for FakeGpuEnumerator {
        fn enumerate_adapters(&self) -> Result<Vec<GpuAdapterInfo>, ClientError> {
            Ok(self.adapters.clone())
        }
    }

    let dir = TempDir::new().unwrap();
    let snapshot_path = dir.path().join("topology-backup.json");
    let driver = Arc::new(FakeDriver::new());
    let launcher = Arc::new(FakeLauncher::new(Some(1)));
    let watcher = Arc::new(FakeProcessWatcher::new());
    let gpus = Arc::new(FakeGpuEnumerator {
        adapters: vec![
            GpuAdapterInfo {
                name: "AMD Radeon Graphics".to_string(),
                adapter_luid_low: 0x101,
                adapter_luid_high: 0,
                vendor_id: 0x1002,
                device_id: 0x164E,
                pci_bus: Some(12),
                pci_device: Some(0),
                pci_function: Some(0),
                dedicated_vram_bytes: 512 * 1024 * 1024,
                is_software: false,
            },
            GpuAdapterInfo {
                name: "NVIDIA GeForce RTX 4090".to_string(),
                adapter_luid_low: 0x202,
                adapter_luid_high: 0,
                vendor_id: 0x10DE,
                device_id: 0x2684,
                pci_bus: Some(1),
                pci_device: Some(0),
                pci_function: Some(0),
                dedicated_vram_bytes: 24 * 1024 * 1024 * 1024,
                is_software: false,
            },
        ],
    });

    let coord = ServiceCoordinator::with_gpu_enumerator(
        ConfigV1::default(),
        Some(snapshot_path.clone()),
        Arc::new(StaticDriverProvider::new(Arc::clone(&driver) as _)),
        Arc::clone(&launcher) as _,
        Arc::clone(&watcher) as _,
        gpus,
    );

    let gpus_resp = roundtrip_ipc(&coord, 10, IpcRequest::ListGpus);
    match gpus_resp {
        IpcResponse::GpuList {
            gpus,
            configured_selector,
            ..
        } => {
            assert_eq!(configured_selector, "auto");
            assert_eq!(gpus.len(), 2);
            assert!(gpus[1].is_best);
            assert!(!gpus[0].is_best);
        }
        other => panic!("unexpected ListGpus response: {other:?}"),
    }

    let sw_resp = roundtrip_ipc(
        &coord,
        11,
        IpcRequest::SwitchGpu(SwitchGpuRequest {
            selector: "pci:1:0.0".to_string(),
        }),
    );
    assert!(matches!(
        sw_resp,
        IpcResponse::GpuSwitched {
            adapter_luid_low: 0x202,
            ..
        }
    ));

    let explicit_guid = uuid::Uuid::from_bytes([0xAB; 16]);
    let add_resp = roundtrip_ipc(
        &coord,
        12,
        IpcRequest::DisplayAdd(DisplayAddRequest {
            client_id: "portrait-side".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            guid: Some(explicit_guid.hyphenated().to_string()),
            gpu: Some("pci:12:0.0".to_string()),
            orientation: Some(DisplayOrientation::Rotate90),
            position: Some((-1080, 0)),
            make_primary: Some(true),
            ..Default::default()
        }),
    );
    assert!(matches!(add_resp, IpcResponse::DisplayAdded { .. }));

    assert!(launcher.ops().iter().any(|op| matches!(
        op,
        LauncherOp::Apply(p)
            if p.rotation == DisplayRotation::Rotate90
                && p.extend_origin == Some((-1080, 0))
                && p.make_virtual_primary
    )));

    let list_resp = roundtrip_ipc(&coord, 13, IpcRequest::DisplayList);
    match list_resp {
        IpcResponse::DisplayList { displays: items } => {
            assert_eq!(items.len(), 1);
            assert_eq!(
                items[0].monitor_guid,
                explicit_guid.hyphenated().to_string()
            );
        }
        other => panic!("unexpected DisplayList response: {other:?}"),
    }

    save_snapshot_atomic(&snapshot_path, &physical_snapshot_fixture(99)).unwrap();
    let rest_resp = roundtrip_ipc(&coord, 14, IpcRequest::RestoreTopology);
    assert!(matches!(
        rest_resp,
        IpcResponse::TopologyRestored {
            restored_from_backup: true,
            departed_displays: 1,
        }
    ));
    assert_eq!(driver.active_count(), 0);
}
