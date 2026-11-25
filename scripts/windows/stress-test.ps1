[CmdletBinding()]
param(
    [string]$CliPath = "target\release\zerovdd.exe",

    [ValidateRange(1, 100000)]
    [int]$Cycles = 500,

    [ValidateRange(1, 64)]
    [int]$ConcurrentClients = 4,

    [ValidateRange(1, 100000)]
    [int]$IoctlIterations = 250,

    [ValidateRange(0, 10000)]
    [int]$MaxHandleDelta = 16,

    [ValidateRange(0, 4096)]
    [int]$MaxPrivateMemDeltaMb = 16,

    [switch]$SkipAclCheck,

    [string]$JsonReport = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path

function Resolve-CliBinary {
    param([string]$PathArg)
    $candidate = if ([System.IO.Path]::IsPathRooted($PathArg)) {
        $PathArg
    } else {
        Join-Path $RepoRoot $PathArg
    }
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
        throw "zerovdd CLI binary not found at '$candidate'. Build release binary or pass -CliPath."
    }
    return (Resolve-Path -LiteralPath $candidate).Path
}

function Assert-ElevatedAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw "scripts\windows\stress-test.ps1 must run from an elevated Administrator PowerShell session."
    }
}

function Invoke-CliJson {
    param(
        [string]$Exe,
        [string[]]$Arguments,
        [switch]$AllowFailure
    )
    $output = $null
    $exitCode = 0
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $output = & $Exe @Arguments 2>&1
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $prevEap
    }
    $text = ($output | ForEach-Object { $_.ToString() }) -join [Environment]::NewLine
    if (-not $AllowFailure -and $exitCode -ne 0) {
        throw "Command '$Exe $($Arguments -join ' ')' failed with exit code ${exitCode}: $text"
    }
    $parsed = $null
    try {
        $parsed = $text | ConvertFrom-Json
    } catch {
        if (-not $AllowFailure) {
            throw "Failed to parse JSON output from '$Exe $($Arguments -join ' ')': $text"
        }
    }
    return [pscustomobject]@{
        ExitCode = $exitCode
        RawText  = $text
        Json     = $parsed
    }
}

function Get-ZeroVddWudfHost {
    param($DoctorReport)
    $matches = @($DoctorReport.wudf_host_processes | Where-Object { $_.hosts_zerovdd_dll -eq $true })
    if ($matches.Count -eq 0) {
        throw "No WUDFHost.exe process hosting zerovdd.dll was found in 'zerovdd doctor --json'."
    }
    return $matches[0]
}

$NativeSource = @"
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;

public static class ZeroVddStressNative
{
    public const string ControlInterfaceGuidStr = "{2963AE53-4CF3-4385-929A-A8348595217E}";
    private const uint CM_GET_DEVICE_INTERFACE_LIST_PRESENT = 0x00000000;
    private const uint CR_SUCCESS = 0x00000000;
    private const uint CR_BUFFER_SMALL = 0x0000001A;

    private const uint GENERIC_READ = 0x80000000;
    private const uint GENERIC_WRITE = 0x40000000;
    private const uint FILE_SHARE_READ = 0x00000001;
    private const uint FILE_SHARE_WRITE = 0x00000002;
    private const uint OPEN_EXISTING = 3;
    private const uint FILE_ATTRIBUTE_NORMAL = 0x00000080;
    private static readonly IntPtr INVALID_HANDLE_VALUE = new IntPtr(-1);

    private const uint TOKEN_DUPLICATE = 0x0002;
    private const uint TOKEN_IMPERSONATE = 0x0004;
    private const uint TOKEN_QUERY = 0x0008;
    private const uint DISABLE_MAX_PRIVILEGE = 0x00000001;
    private const int ERROR_ACCESS_DENIED = 5;

    [StructLayout(LayoutKind.Sequential)]
    private struct SID_AND_ATTRIBUTES
    {
        public IntPtr Sid;
        public uint Attributes;
    }

    [DllImport("cfgmgr32.dll", CharSet = CharSet.Unicode)]
    private static extern uint CM_Get_Device_Interface_List_SizeW(
        out uint pulLen,
        ref Guid InterfaceClassGuid,
        string pDeviceID,
        uint ulFlags);

    [DllImport("cfgmgr32.dll", CharSet = CharSet.Unicode)]
    private static extern uint CM_Get_Device_Interface_ListW(
        ref Guid InterfaceClassGuid,
        string pDeviceID,
        char[] Buffer,
        uint BufferLen,
        uint ulFlags);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateFileW(
        string lpFileName,
        uint dwDesiredAccess,
        uint dwShareMode,
        IntPtr lpSecurityAttributes,
        uint dwCreationDisposition,
        uint dwFlagsAndAttributes,
        IntPtr hTemplateFile);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool DeviceIoControl(
        IntPtr hDevice,
        uint dwIoControlCode,
        byte[] lpInBuffer,
        uint nInBufferSize,
        byte[] lpOutBuffer,
        uint nOutBufferSize,
        out uint lpBytesReturned,
        IntPtr lpOverlapped);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr hObject);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(
        uint dwDesiredAccess,
        bool bInheritHandle,
        int dwProcessId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool DuplicateHandle(
        IntPtr hSourceProcessHandle,
        IntPtr hSourceHandle,
        IntPtr hTargetProcessHandle,
        out IntPtr lpTargetHandle,
        uint dwDesiredAccess,
        bool bInheritHandle,
        uint dwOptions);

    [DllImport("ntdll.dll")]
    private static extern int NtQueryInformationProcess(
        IntPtr ProcessHandle,
        int ProcessInformationClass,
        IntPtr ProcessInformation,
        int ProcessInformationLength,
        out int ReturnLength);

    [DllImport("ntdll.dll")]
    private static extern int NtQueryObject(
        IntPtr Handle,
        int ObjectInformationClass,
        IntPtr ObjectInformation,
        int ObjectInformationLength,
        out int ReturnLength);

    [StructLayout(LayoutKind.Sequential)]
    private struct PROCESS_HANDLE_TABLE_ENTRY_INFO
    {
        public IntPtr HandleValue;
        public UIntPtr HandleCount;
        public UIntPtr PointerCount;
        public uint GrantedAccess;
        public uint ObjectTypeIndex;
        public uint HandleAttributes;
        public uint Reserved;
    }

    public sealed class WudfHandleBreakdown
    {
        public int TotalHandles;
        public int DriverOwnedHandles;
        public int DxgkSharedSwapChainHandles;
        public int DxgkSharedSyncHandles;
        public int EventHandles;
        public int ThreadHandles;
    }

    public static WudfHandleBreakdown QueryWudfHandles(int pid)
    {
        const uint PROCESS_QUERY_DUP = 0x0400 | 0x0040;
        const int ProcessHandleInformation = 51;
        const int ObjectTypeInformation = 2;
        const int STATUS_INFO_LENGTH_MISMATCH = unchecked((int)0xC0000004);

        IntPtr proc = OpenProcess(PROCESS_QUERY_DUP, false, pid);
        if (proc == IntPtr.Zero)
        {
            throw new Win32Exception(Marshal.GetLastWin32Error(), "OpenProcess failed for WUDFHost PID " + pid);
        }

        IntPtr snapshotBuf = IntPtr.Zero;
        IntPtr typeBuf = IntPtr.Zero;
        try
        {
            int bufSize = 256 * 1024;
            snapshotBuf = Marshal.AllocHGlobal(bufSize);
            int retLen = 0;
            int status = NtQueryInformationProcess(proc, ProcessHandleInformation, snapshotBuf, bufSize, out retLen);
            for (int retry = 0; retry < 4 && status == STATUS_INFO_LENGTH_MISMATCH; retry++)
            {
                Marshal.FreeHGlobal(snapshotBuf);
                snapshotBuf = IntPtr.Zero;
                bufSize = Math.Max(retLen + 65536, bufSize * 2);
                snapshotBuf = Marshal.AllocHGlobal(bufSize);
                status = NtQueryInformationProcess(proc, ProcessHandleInformation, snapshotBuf, bufSize, out retLen);
            }
            if (status != 0)
            {
                throw new InvalidOperationException("NtQueryInformationProcess(51) failed with NTSTATUS 0x" + status.ToString("X8"));
            }

            long count = IntPtr.Size == 8 ? Marshal.ReadInt64(snapshotBuf, 0) : Marshal.ReadInt32(snapshotBuf, 0);
            int headerSize = IntPtr.Size * 2;
            int entrySize = Marshal.SizeOf(typeof(PROCESS_HANDLE_TABLE_ENTRY_INFO));
            typeBuf = Marshal.AllocHGlobal(4096);
            Dictionary<uint, string> typeNames = new Dictionary<uint, string>();

            WudfHandleBreakdown result = new WudfHandleBreakdown();
            result.TotalHandles = checked((int)count);

            for (long i = 0; i < count; i++)
            {
                IntPtr entryPtr = IntPtr.Add(snapshotBuf, checked((int)(headerSize + i * entrySize)));
                PROCESS_HANDLE_TABLE_ENTRY_INFO entry = (PROCESS_HANDLE_TABLE_ENTRY_INFO)Marshal.PtrToStructure(
                    entryPtr,
                    typeof(PROCESS_HANDLE_TABLE_ENTRY_INFO));

                string typeName;
                if (!typeNames.TryGetValue(entry.ObjectTypeIndex, out typeName))
                {
                    typeName = "Type#" + entry.ObjectTypeIndex;
                    IntPtr dupHandle;
                    if (DuplicateHandle(proc, entry.HandleValue, GetCurrentProcess(), out dupHandle, 0, false, 0))
                    {
                        try
                        {
                            int tRet = 0;
                            if (NtQueryObject(dupHandle, ObjectTypeInformation, typeBuf, 4096, out tRet) == 0)
                            {
                                ushort uniLen = (ushort)Marshal.ReadInt16(typeBuf, 0);
                                IntPtr uniBuf = Marshal.ReadIntPtr(typeBuf, IntPtr.Size == 8 ? 8 : 4);
                                if (uniLen > 0 && uniBuf != IntPtr.Zero)
                                {
                                    typeName = Marshal.PtrToStringUni(uniBuf, uniLen / 2);
                                    typeNames[entry.ObjectTypeIndex] = typeName;
                                }
                            }
                        }
                        finally
                        {
                            CloseHandle(dupHandle);
                        }
                    }
                }

                if (typeName == "DxgkSharedSwapChainObject")
                {
                    result.DxgkSharedSwapChainHandles++;
                }
                else if (typeName == "DxgkSharedSyncObject")
                {
                    result.DxgkSharedSyncHandles++;
                }
                else if (typeName == "Event")
                {
                    result.EventHandles++;
                }
                else if (typeName == "Thread")
                {
                    result.ThreadHandles++;
                }
            }

            result.DriverOwnedHandles = result.TotalHandles
                - result.DxgkSharedSwapChainHandles
                - result.DxgkSharedSyncHandles;
            return result;
        }
        finally
        {
            if (typeBuf != IntPtr.Zero) Marshal.FreeHGlobal(typeBuf);
            if (snapshotBuf != IntPtr.Zero) Marshal.FreeHGlobal(snapshotBuf);
            CloseHandle(proc);
        }
    }

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool OpenProcessToken(
        IntPtr ProcessHandle,
        uint DesiredAccess,
        out IntPtr TokenHandle);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool CreateRestrictedToken(
        IntPtr ExistingTokenHandle,
        uint Flags,
        uint DisableSidCount,
        [In] SID_AND_ATTRIBUTES[] SidsToDisable,
        uint DeletePrivilegeCount,
        IntPtr PrivilegesToDelete,
        uint RestrictedSidCount,
        IntPtr SidsToRestrict,
        out IntPtr NewTokenHandle);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool ImpersonateLoggedOnUser(IntPtr hToken);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool RevertToSelf();

    public static uint CtlCode(uint deviceType, uint function, uint method, uint access)
    {
        return ((deviceType & 0xFFFF) << 16)
            | ((access & 0x3) << 14)
            | ((function & 0x0FFF) << 2)
            | (method & 0x3);
    }

    public static string ResolveControlDevicePath()
    {
        Guid guid = new Guid("2963AE53-4CF3-4385-929A-A8348595217E");
        uint len = 0;
        for (int attempt = 0; attempt < 4; attempt++)
        {
            uint cr = CM_Get_Device_Interface_List_SizeW(out len, ref guid, null, CM_GET_DEVICE_INTERFACE_LIST_PRESENT);
            if (cr != CR_SUCCESS)
            {
                throw new InvalidOperationException("CM_Get_Device_Interface_List_SizeW failed with ConfigMgr code 0x" + cr.ToString("X8"));
            }
            if (len <= 1)
            {
                throw new InvalidOperationException("No present zerovdd Control Device interface found for GUID " + ControlInterfaceGuidStr);
            }
            char[] buf = new char[len];
            cr = CM_Get_Device_Interface_ListW(ref guid, null, buf, len, CM_GET_DEVICE_INTERFACE_LIST_PRESENT);
            if (cr == CR_BUFFER_SMALL)
            {
                continue;
            }
            if (cr != CR_SUCCESS)
            {
                throw new InvalidOperationException("CM_Get_Device_Interface_ListW failed with ConfigMgr code 0x" + cr.ToString("X8"));
            }
            string multi = new string(buf);
            string[] entries = multi.Split(new char[] { '\0' }, StringSplitOptions.RemoveEmptyEntries);
            foreach (string entry in entries)
            {
                if (entry.EndsWith(@"\ZeroVddControl", StringComparison.OrdinalIgnoreCase))
                {
                    return entry;
                }
            }
            throw new InvalidOperationException(@"No device interface entry ending in \ZeroVddControl found for GUID " + ControlInterfaceGuidStr);
        }
        throw new InvalidOperationException("Exhausted retries enumerating device interfaces for GUID " + ControlInterfaceGuidStr);
    }

    public sealed class IoctlFuzzSummary
    {
        public int TotalCalls;
        public int ExpectedRejections;
        public int UnexpectedSuccesses;
        public bool PostFuzzGetVersionOk;
        public bool PostFuzzGetStatusOk;
    }

    public static IoctlFuzzSummary RunIoctlFuzz(string devicePath, int iterations)
    {
        uint ioctlGetVersion = CtlCode(0x0022, 0x800, 0, 1);
        uint ioctlAddDisplay = CtlCode(0x0022, 0x801, 0, 3);
        uint ioctlRemoveDisplay = CtlCode(0x0022, 0x802, 0, 3);
        uint ioctlListDisplays = CtlCode(0x0022, 0x803, 0, 1);
        uint ioctlSetRenderAdapter = CtlCode(0x0022, 0x804, 0, 3);
        uint ioctlRenewLease = CtlCode(0x0022, 0x805, 0, 3);
        uint ioctlGetStatus = CtlCode(0x0022, 0x806, 0, 1);
        uint ioctlUnknown = CtlCode(0x0022, 0x8FF, 0, 3);

        IntPtr handle = CreateFileW(
            devicePath,
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            IntPtr.Zero,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            IntPtr.Zero);
        if (handle == INVALID_HANDLE_VALUE)
        {
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Failed to open Control Device for IOCTL fuzzing: " + devicePath);
        }

        try
        {
            IoctlFuzzSummary summary = new IoctlFuzzSummary();
            Random rng = new Random(0x52564444);

            uint[] allCodes = new uint[]
            {
                ioctlGetVersion,
                ioctlAddDisplay,
                ioctlRemoveDisplay,
                ioctlListDisplays,
                ioctlSetRenderAdapter,
                ioctlRenewLease,
                ioctlGetStatus,
                ioctlUnknown,
                0xDEADBEEF
            };

            List<Tuple<uint, byte[], int>> edgeCases = new List<Tuple<uint, byte[], int>>();

            // Undersized buffers across all IOCTLs.
            foreach (uint code in allCodes)
            {
                edgeCases.Add(Tuple.Create(code, new byte[0], 0));
                edgeCases.Add(Tuple.Create(code, new byte[1], 1));
                edgeCases.Add(Tuple.Create(code, new byte[7], 7));
            }

            // Oversized 4096-byte input buffers on input-validating and unknown IOCTLs.
            uint[] inputValidatingCodes = new uint[]
            {
                ioctlAddDisplay,
                ioctlRemoveDisplay,
                ioctlSetRenderAdapter,
                ioctlRenewLease,
                ioctlUnknown,
                0xDEADBEEF
            };
            foreach (uint code in inputValidatingCodes)
            {
                edgeCases.Add(Tuple.Create(code, new byte[4096], 4096));
            }

            // AddDisplayRequest (656 bytes) crafted malformed payloads.
            byte[] badVersionAdd = BuildBaseAddDisplayRequest();
            badVersionAdd[28] = 99;
            badVersionAdd[29] = 0;
            edgeCases.Add(Tuple.Create(ioctlAddDisplay, badVersionAdd, 20));

            byte[] zeroDimAdd = BuildBaseAddDisplayRequest();
            Array.Clear(zeroDimAdd, 0, 8);
            edgeCases.Add(Tuple.Create(ioctlAddDisplay, zeroDimAdd, 20));

            byte[] badUtf8Add = BuildBaseAddDisplayRequest();
            badUtf8Add[62] = 4;
            badUtf8Add[64] = 0xFF;
            badUtf8Add[65] = 0xFE;
            edgeCases.Add(Tuple.Create(ioctlAddDisplay, badUtf8Add, 20));

            byte[] badNameLenAdd = BuildBaseAddDisplayRequest();
            badNameLenAdd[62] = 255;
            edgeCases.Add(Tuple.Create(ioctlAddDisplay, badNameLenAdd, 20));

            byte[] badEdidLenAdd = BuildBaseAddDisplayRequest();
            badEdidLenAdd[36] = 64;
            badEdidLenAdd[37] = 0;
            edgeCases.Add(Tuple.Create(ioctlAddDisplay, badEdidLenAdd, 20));

            byte[] oversizedAdd = new byte[657];
            Array.Copy(BuildBaseAddDisplayRequest(), oversizedAdd, 656);
            edgeCases.Add(Tuple.Create(ioctlAddDisplay, oversizedAdd, 20));

            // RemoveDisplayRequest (32 bytes) zero selector and bad flags.
            byte[] zeroRemove = new byte[32];
            zeroRemove[0] = 1;
            edgeCases.Add(Tuple.Create(ioctlRemoveDisplay, zeroRemove, 0));

            byte[] badFlagsRemove = new byte[32];
            badFlagsRemove[0] = 1;
            badFlagsRemove[28] = 0x02;
            edgeCases.Add(Tuple.Create(ioctlRemoveDisplay, badFlagsRemove, 0));

            // RenewLeaseRequest (32 bytes) zero TTL and zero GUID.
            byte[] zeroTtlRenew = new byte[32];
            zeroTtlRenew[0] = 1;
            zeroTtlRenew[8] = 0x11;
            edgeCases.Add(Tuple.Create(ioctlRenewLease, zeroTtlRenew, 0));

            foreach (var tc in edgeCases)
            {
                summary.TotalCalls++;
                byte[] outBuf = tc.Item3 > 0 ? new byte[tc.Item3] : null;
                uint returned = 0;
                bool ok = DeviceIoControl(
                    handle,
                    tc.Item1,
                    tc.Item2,
                    (uint)tc.Item2.Length,
                    outBuf,
                    (uint)tc.Item3,
                    out returned,
                    IntPtr.Zero);
                if (ok)
                {
                    summary.UnexpectedSuccesses++;
                }
                else
                {
                    summary.ExpectedRejections++;
                }
            }

            uint[] mutatingFuzzCodes = new uint[]
            {
                ioctlAddDisplay,
                ioctlRemoveDisplay,
                ioctlRenewLease,
                ioctlUnknown,
                0xDEADBEEF
            };
            int[] fuzzSizes = new int[] { 0, 3, 19, 31, 32, 33, 651, 652, 653, 1024 };

            for (int i = 0; i < iterations; i++)
            {
                summary.TotalCalls++;
                uint code = mutatingFuzzCodes[i % mutatingFuzzCodes.Length];
                int len = fuzzSizes[rng.Next(fuzzSizes.Length)];
                byte[] inBuf = new byte[len];
                rng.NextBytes(inBuf);
                // Force invalid major version on fixed-size matches so valid accidental requests never arrive.
                if (len >= 2 && code != ioctlAddDisplay)
                {
                    inBuf[0] = 0xFE;
                    inBuf[1] = 0x00;
                }
                if (len >= 30 && code == ioctlAddDisplay)
                {
                    inBuf[28] = 0xFE;
                    inBuf[29] = 0x00;
                }
                byte[] outBuf = new byte[128];
                uint returned = 0;
                bool ok = DeviceIoControl(
                    handle,
                    code,
                    inBuf,
                    (uint)inBuf.Length,
                    outBuf,
                    (uint)outBuf.Length,
                    out returned,
                    IntPtr.Zero);
                if (ok)
                {
                    summary.UnexpectedSuccesses++;
                }
                else
                {
                    summary.ExpectedRejections++;
                }
            }

            // Confirm driver still responds cleanly to IOCTL_GET_VERSION and IOCTL_GET_STATUS.
            byte[] verOut = new byte[8];
            uint verBytes = 0;
            summary.PostFuzzGetVersionOk = DeviceIoControl(
                handle,
                ioctlGetVersion,
                null,
                0,
                verOut,
                (uint)verOut.Length,
                out verBytes,
                IntPtr.Zero) && verBytes == 8;

            byte[] statusOut = new byte[56];
            uint statusBytes = 0;
            summary.PostFuzzGetStatusOk = DeviceIoControl(
                handle,
                ioctlGetStatus,
                null,
                0,
                statusOut,
                (uint)statusOut.Length,
                out statusBytes,
                IntPtr.Zero) && statusBytes == 56;

            return summary;
        }
        finally
        {
            CloseHandle(handle);
        }
    }

    public static int TestInvalidRenderAdapterIoctl(string devicePath)
    {
        uint ioctlSetRenderAdapter = CtlCode(0x0022, 0x804, 0, 3);
        IntPtr handle = CreateFileW(
            devicePath,
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            IntPtr.Zero,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            IntPtr.Zero);
        if (handle == INVALID_HANDLE_VALUE)
        {
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Failed to open Control Device for GPU LUID test.");
        }

        try
        {
            byte[] req = new byte[20];
            // ProtocolVersion 1.0.0
            req[0] = 1;
            req[6] = 3;
            // Bogus adapter LUID (0xDEADBEEF, 0x12345678)
            req[8] = 0xEF; req[9] = 0xBE; req[10] = 0xAD; req[11] = 0xDE;
            req[12] = 0x78; req[13] = 0x56; req[14] = 0x34; req[15] = 0x12;
            uint returned = 0;
            bool ok = DeviceIoControl(handle, ioctlSetRenderAdapter, req, 20, null, 0, out returned, IntPtr.Zero);
            return ok ? 0 : Marshal.GetLastWin32Error();
        }
        finally
        {
            CloseHandle(handle);
        }
    }

    public static int TestUnprivilegedAclDenial(string devicePath)
    {
        IntPtr processToken = IntPtr.Zero;
        IntPtr restrictedToken = IntPtr.Zero;
        IntPtr adminSidPtr = IntPtr.Zero;

        try
        {
            if (!OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_IMPERSONATE | TOKEN_QUERY, out processToken))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "OpenProcessToken failed.");
            }

            SecurityIdentifier adminSid = new SecurityIdentifier(WellKnownSidType.BuiltinAdministratorsSid, null);
            byte[] sidBytes = new byte[adminSid.BinaryLength];
            adminSid.GetBinaryForm(sidBytes, 0);
            adminSidPtr = Marshal.AllocHGlobal(sidBytes.Length);
            Marshal.Copy(sidBytes, 0, adminSidPtr, sidBytes.Length);

            SID_AND_ATTRIBUTES[] disableSids = new SID_AND_ATTRIBUTES[]
            {
                new SID_AND_ATTRIBUTES { Sid = adminSidPtr, Attributes = 0 }
            };

            if (!CreateRestrictedToken(
                processToken,
                DISABLE_MAX_PRIVILEGE,
                1,
                disableSids,
                0,
                IntPtr.Zero,
                0,
                IntPtr.Zero,
                out restrictedToken))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "CreateRestrictedToken failed.");
            }

            if (!ImpersonateLoggedOnUser(restrictedToken))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "ImpersonateLoggedOnUser failed.");
            }

            try
            {
                IntPtr handle = CreateFileW(
                    devicePath,
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    IntPtr.Zero,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    IntPtr.Zero);
                if (handle != INVALID_HANDLE_VALUE)
                {
                    CloseHandle(handle);
                    return 0;
                }
                return Marshal.GetLastWin32Error();
            }
            finally
            {
                RevertToSelf();
            }
        }
        finally
        {
            if (adminSidPtr != IntPtr.Zero) Marshal.FreeHGlobal(adminSidPtr);
            if (restrictedToken != IntPtr.Zero) CloseHandle(restrictedToken);
            if (processToken != IntPtr.Zero) CloseHandle(processToken);
        }
    }

    private static byte[] BuildBaseAddDisplayRequest()
    {
        byte[] buf = new byte[656];
        // width = 1920, height = 1080, refresh_num = 60, refresh_den = 1
        BitConverter.GetBytes((uint)1920).CopyTo(buf, 0);
        BitConverter.GetBytes((uint)1080).CopyTo(buf, 4);
        BitConverter.GetBytes((uint)60).CopyTo(buf, 8);
        BitConverter.GetBytes((uint)1).CopyTo(buf, 12);
        // protocol_version = 1.1.0 flags=15 at offset 28
        BitConverter.GetBytes((ushort)1).CopyTo(buf, 28);
        BitConverter.GetBytes((ushort)1).CopyTo(buf, 30);
        BitConverter.GetBytes((ushort)15).CopyTo(buf, 34);
        // non-zero monitor_guid at offset 44..60
        buf[44] = 0xAB;
        buf[59] = 0xCD;
        return buf;
    }
}
"@

Assert-ElevatedAdministrator
$ResolvedCli = Resolve-CliBinary -PathArg $CliPath

if (-not ("ZeroVddStressNative" -as [type])) {
    Add-Type -TypeDefinition $NativeSource -Language CSharp
}

Write-Host "[1/5] Verifying driver health and recording WUDFHost.exe baseline..."
$preflightDoctor = Invoke-CliJson -Exe $ResolvedCli -Arguments @("doctor", "--json")
if ($preflightDoctor.Json.control_device.state -ne "accessible") {
    throw "Control Device is not accessible (state: $($preflightDoctor.Json.control_device.state)). Install and start the driver first."
}
# Perform 1 warm-up cycle so one-time D3D11/DXGI/threadpool initialization settles before baseline.
$warmupAdd = Invoke-CliJson -Exe $ResolvedCli -Arguments @("display", "add", "1920x1080@60", "--name", "soak-warmup", "--lease", "120", "--json")
if ([string]::IsNullOrWhiteSpace($warmupAdd.Json.monitor_guid)) {
    throw "Warm-up display add did not return monitor_guid."
}
Start-Sleep -Milliseconds 350
$null = Invoke-CliJson -Exe $ResolvedCli -Arguments @("display", "remove", "--guid", $warmupAdd.Json.monitor_guid, "--json")
Start-Sleep -Milliseconds 300

$baselineDoctor = Invoke-CliJson -Exe $ResolvedCli -Arguments @("doctor", "--json")
if ($baselineDoctor.Json.control_device.state -ne "accessible") {
    throw "Control Device is not accessible after warm-up (state: $($baselineDoctor.Json.control_device.state))."
}
$baselineWudf = Get-ZeroVddWudfHost -DoctorReport $baselineDoctor.Json
$baselinePid = [int]$baselineWudf.pid
$baselineHandles = [int]$baselineWudf.handle_count
$baselinePrivateMb = [math]::Round(($baselineWudf.private_memory_bytes / 1MB), 2)
$baselineBreakdown = [ZeroVddStressNative]::QueryWudfHandles($baselinePid)
$ControlDevicePath = [ZeroVddStressNative]::ResolveControlDevicePath()

Write-Host ("  Baseline WUDFHost PID={0} Handles={1} DriverOwnedHandles={2} PrivateMem={3} MB Path={4}" -f $baselinePid, $baselineHandles, $baselineBreakdown.DriverOwnedHandles, $baselinePrivateMb, $ControlDevicePath)

Write-Host "[2/5] Running $Cycles sequential add/remove cycles + $ConcurrentClients concurrent client workers..."
$modeRotation = @(
    @{ Mode = "1920x1080@60"; Hdr = $false },
    @{ Mode = "2560x1600@90"; Hdr = $false },
    @{ Mode = "3840x2160@59.94"; Hdr = $false },
    @{ Mode = "3840x2160@120"; Hdr = $true }
)

for ($i = 0; $i -lt $Cycles; $i++) {
    $entry = $modeRotation[$i % $modeRotation.Count]
    $name = "soak-$i"
    $addArgs = @("display", "add", $entry.Mode, "--name", $name, "--lease", "120", "--json")
    if ($entry.Hdr) {
        $addArgs += "--hdr"
    }
    $guid = $null
    $cycleFailed = $false
    try {
        $addRes = Invoke-CliJson -Exe $ResolvedCli -Arguments $addArgs
        $guid = $addRes.Json.monitor_guid
        if ([string]::IsNullOrWhiteSpace($guid)) {
            throw "Cycle ${i}: display add did not return monitor_guid."
        }
    } catch {
        $cycleFailed = $true
        throw
    } finally {
        if (-not [string]::IsNullOrWhiteSpace($guid)) {
            if ($cycleFailed) {
                $null = Invoke-CliJson -Exe $ResolvedCli -Arguments @("display", "remove", "--guid", $guid, "--json") -AllowFailure
            } else {
                $null = Invoke-CliJson -Exe $ResolvedCli -Arguments @("display", "remove", "--guid", $guid, "--json")
            }
        }
    }

    if ((($i + 1) % 50) -eq 0 -or ($i + 1) -eq $Cycles) {
        Write-Host ("  Completed {0}/{1} sequential cycles" -f ($i + 1), $Cycles)
    }
}

$jobs = @()
for ($w = 0; $w -lt $ConcurrentClients; $w++) {
    $jobs += Start-Job -ScriptBlock {
        param($Exe, $WorkerId)
        Set-StrictMode -Version Latest
        $ErrorActionPreference = "Stop"
        $modes = @("1920x1080@60", "2560x1600@90", "3840x2160@59.94", "3840x2160@120")
        for ($iter = 0; $iter -lt 5; $iter++) {
            $name = "stress-w$WorkerId-$iter"
            $mode = $modes[$iter % $modes.Count]
            $addArgs = @("display", "add", $mode, "--name", $name, "--lease", "120", "--json")
            if ($mode -eq "3840x2160@120") { $addArgs += "--hdr" }
            $prevEap = $ErrorActionPreference
            $ErrorActionPreference = "Continue"
            try {
                $addOut = & $Exe @addArgs 2>&1
                $addCode = $LASTEXITCODE
            } finally {
                $ErrorActionPreference = $prevEap
            }
            if ($addCode -ne 0) {
                throw "Worker $WorkerId iter $iter add failed: $($addOut -join ' ')"
            }
            $added = ($addOut | ForEach-Object { $_.ToString() }) -join [Environment]::NewLine | ConvertFrom-Json
            $prevEap = $ErrorActionPreference
            $ErrorActionPreference = "Continue"
            try {
                $listOut = & $Exe display list --json 2>&1
                $listCode = $LASTEXITCODE
            } finally {
                $ErrorActionPreference = $prevEap
            }
            if ($listCode -ne 0) {
                throw "Worker $WorkerId iter $iter list failed: $($listOut -join ' ')"
            }
            $prevEap = $ErrorActionPreference
            $ErrorActionPreference = "Continue"
            try {
                $remOut = & $Exe display remove --guid $added.monitor_guid --json 2>&1
                $remCode = $LASTEXITCODE
            } finally {
                $ErrorActionPreference = $prevEap
            }
            if ($remCode -ne 0) {
                throw "Worker $WorkerId iter $iter remove failed: $($remOut -join ' ')"
            }
        }
    } -ArgumentList $ResolvedCli, $w
}

$null = Wait-Job -Job $jobs
$workerErrors = @()
foreach ($job in $jobs) {
    if ($job.State -ne "Completed") {
        $workerErrors += "Job $($job.Id) state=$($job.State)"
    }
    try {
        $null = Receive-Job -Job $job -ErrorAction Stop
    } catch {
        $workerErrors += $_.Exception.Message
    }
}
Remove-Job -Job $jobs -Force
if ($workerErrors.Count -gt 0) {
    throw "Concurrent client stress failed: $($workerErrors -join '; ')"
}

$postSoakDoctor = Invoke-CliJson -Exe $ResolvedCli -Arguments @("doctor", "--json")
$postSoakWudf = Get-ZeroVddWudfHost -DoctorReport $postSoakDoctor.Json
$postSoakPid = [int]$postSoakWudf.pid
$postSoakBreakdown = [ZeroVddStressNative]::QueryWudfHandles($postSoakPid)
$totalHandleDelta = [int]$postSoakWudf.handle_count - $baselineHandles
$driverHandleDelta = $postSoakBreakdown.DriverOwnedHandles - $baselineBreakdown.DriverOwnedHandles
$dxgkSwapChainDelta = $postSoakBreakdown.DxgkSharedSwapChainHandles - $baselineBreakdown.DxgkSharedSwapChainHandles
$dxgkSyncDelta = $postSoakBreakdown.DxgkSharedSyncHandles - $baselineBreakdown.DxgkSharedSyncHandles
$eventHandleDelta = $postSoakBreakdown.EventHandles - $baselineBreakdown.EventHandles
$threadHandleDelta = $postSoakBreakdown.ThreadHandles - $baselineBreakdown.ThreadHandles
$privateMemDeltaMb = [math]::Round((([double]$postSoakWudf.private_memory_bytes - [double]$baselineWudf.private_memory_bytes) / 1MB), 2)

if ($postSoakPid -ne $baselinePid) {
    throw "WUDFHost.exe PID changed during soak (baseline=$baselinePid, post=$postSoakPid), indicating a host crash."
}
if ($driverHandleDelta -gt $MaxHandleDelta) {
    throw "WUDFHost.exe driver-owned handle delta $driverHandleDelta exceeded threshold $MaxHandleDelta (totalDelta=$totalHandleDelta, dxgkSwapChainDelta=$dxgkSwapChainDelta, dxgkSyncDelta=$dxgkSyncDelta, eventDelta=$eventHandleDelta, threadDelta=$threadHandleDelta)."
}
if ($threadHandleDelta -gt 2) {
    throw "WUDFHost.exe Thread handle delta $threadHandleDelta exceeded threshold 2."
}
if ($privateMemDeltaMb -gt $MaxPrivateMemDeltaMb) {
    throw "WUDFHost.exe private working set delta ${privateMemDeltaMb} MB exceeded threshold ${MaxPrivateMemDeltaMb} MB."
}

Write-Host ("  Post-soak WUDFHost PID={0} DriverHandleDelta={1} TotalHandleDelta={2} (DxgkSwapChain={3}, DxgkSync={4}, Event={5}, Thread={6}) PrivateMemDelta={7} MB" -f $postSoakPid, $driverHandleDelta, $totalHandleDelta, $dxgkSwapChainDelta, $dxgkSyncDelta, $eventHandleDelta, $threadHandleDelta, $privateMemDeltaMb)

Write-Host "[3/5] Running raw IOCTL fuzzing ($IoctlIterations iterations + malformed vectors)..."
$fuzzSummary = [ZeroVddStressNative]::RunIoctlFuzz($ControlDevicePath, $IoctlIterations)
if ($fuzzSummary.UnexpectedSuccesses -gt 0) {
    throw "IOCTL fuzzing observed $($fuzzSummary.UnexpectedSuccesses) unexpected successes on malformed inputs."
}
if (-not $fuzzSummary.PostFuzzGetVersionOk -or -not $fuzzSummary.PostFuzzGetStatusOk) {
    throw "Control Device failed health check after IOCTL fuzzing."
}

$postFuzzDoctor = Invoke-CliJson -Exe $ResolvedCli -Arguments @("doctor", "--json")
$postFuzzWudf = Get-ZeroVddWudfHost -DoctorReport $postFuzzDoctor.Json
if ([int]$postFuzzWudf.pid -ne $baselinePid) {
    throw "WUDFHost.exe crashed during IOCTL fuzzing (baseline=$baselinePid, post=$($postFuzzWudf.pid))."
}
Write-Host ("  IOCTL fuzz passed: TotalCalls={0} Rejected={1} WUDFHostPID={2}" -f $fuzzSummary.TotalCalls, $fuzzSummary.ExpectedRejections, $postFuzzWudf.pid)

Write-Host "[4/5] Running rapid GPU-switch and invalid LUID safety checks..."
$gpuSwitchCount = 0
for ($g = 0; $g -lt 5; $g++) {
    $null = Invoke-CliJson -Exe $ResolvedCli -Arguments @("display", "switch-gpu", "auto", "--json")
    $gpuSwitchCount++
}
foreach ($gpu in @($baselineDoctor.Json.gpus)) {
    if (-not $gpu.is_software) {
        $luidSelector = "luid:{0}:{1}" -f $gpu.adapter_luid_low, $gpu.adapter_luid_high
        $null = Invoke-CliJson -Exe $ResolvedCli -Arguments @("display", "switch-gpu", $luidSelector, "--json")
        $gpuSwitchCount++
    }
}
$invalidCliGpu = Invoke-CliJson -Exe $ResolvedCli -Arguments @("display", "switch-gpu", "luid:999999999:999999999", "--json") -AllowFailure
if ($invalidCliGpu.ExitCode -eq 0) {
    throw "Expected invalid GPU LUID selector to fail with non-zero exit code, but it exited 0."
}
$invalidIoctlErr = [ZeroVddStressNative]::TestInvalidRenderAdapterIoctl($ControlDevicePath)
$null = Invoke-CliJson -Exe $ResolvedCli -Arguments @("display", "switch-gpu", "auto", "--json")
$gpuSwitchCount++

$postGpuDoctor = Invoke-CliJson -Exe $ResolvedCli -Arguments @("doctor", "--json")
$postGpuWudf = Get-ZeroVddWudfHost -DoctorReport $postGpuDoctor.Json
if ([int]$postGpuWudf.pid -ne $baselinePid) {
    throw "WUDFHost.exe crashed during GPU-switch stress (baseline=$baselinePid, post=$($postGpuWudf.pid))."
}
Write-Host ("  GPU-switch stress passed: Switches={0} InvalidCliExit={1} InvalidIoctlErr={2}" -f $gpuSwitchCount, $invalidCliGpu.ExitCode, $invalidIoctlErr)

$aclCheckStatus = "skipped"
$aclErrorCode = $null
if (-not $SkipAclCheck) {
    Write-Host "[5/5] Verifying non-admin Control Device ACL denial (ERROR_ACCESS_DENIED = 5)..."
    $aclErrorCode = [ZeroVddStressNative]::TestUnprivilegedAclDenial($ControlDevicePath)
    if ($aclErrorCode -ne 5) {
        throw "Unprivileged CreateFileW on '$ControlDevicePath' returned Win32 error $aclErrorCode; expected ERROR_ACCESS_DENIED (5)."
    }
    $aclCheckStatus = "passed"
    Write-Host "  Non-admin ACL check passed: ERROR_ACCESS_DENIED (5) enforced on Control Device interface."
} else {
    Write-Host "[5/5] Skipping non-admin ACL check (-SkipAclCheck)."
}

$report = [ordered]@{
    status                      = "passed"
    cli_path                    = $ResolvedCli
    control_device_path         = $ControlDevicePath
    wudf_host_pid               = $baselinePid
    baseline_handles            = $baselineHandles
    baseline_driver_handles     = $baselineBreakdown.DriverOwnedHandles
    post_soak_handles           = [int]$postSoakWudf.handle_count
    post_soak_driver_handles    = $postSoakBreakdown.DriverOwnedHandles
    handle_delta                = $totalHandleDelta
    driver_handle_delta         = $driverHandleDelta
    dxgk_shared_swapchain_delta = $dxgkSwapChainDelta
    dxgk_shared_sync_delta      = $dxgkSyncDelta
    event_handle_delta          = $eventHandleDelta
    thread_handle_delta         = $threadHandleDelta
    baseline_private_mb         = $baselinePrivateMb
    private_mem_delta_mb        = $privateMemDeltaMb
    sequential_cycles           = $Cycles
    concurrent_clients          = $ConcurrentClients
    ioctl_fuzz_calls            = $fuzzSummary.TotalCalls
    ioctl_fuzz_rejections       = $fuzzSummary.ExpectedRejections
    gpu_switches                = $gpuSwitchCount
    invalid_luid_ioctl_err      = $invalidIoctlErr
    acl_check                   = $aclCheckStatus
    acl_win32_error             = $aclErrorCode
}

if (-not [string]::IsNullOrWhiteSpace($JsonReport)) {
    $reportParent = Split-Path -Path $JsonReport -Parent
    if (-not [string]::IsNullOrWhiteSpace($reportParent) -and -not (Test-Path -LiteralPath $reportParent)) {
        $null = New-Item -ItemType Directory -Force -Path $reportParent
    }
    $report | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $JsonReport -Encoding UTF8
    Write-Host "Wrote stress report to $JsonReport"
}

Write-Host "ALL STRESS CHECKS PASSED."
