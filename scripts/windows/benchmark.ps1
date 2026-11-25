[CmdletBinding()]
param(
    [string]$CliPath = "",

    [string]$DisplayName = "",

    [string]$Mode = "2560x1440@120",

    [switch]$Hdr,

    [ValidateRange(2, 3600)]
    [int]$DurationSeconds = 15,

    [ValidateRange(0, 60)]
    [int]$WarmupSeconds = 2,

    [switch]$NoStimulus,

    [switch]$UseExplorerToken,

    [switch]$CaptureEtw,

    [switch]$KeepEtl,

    [switch]$Force,

    [string]$JsonReport = "",

    [switch]$WorkerMode,

    [string]$WorkerOutputPath = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$ZeroVddEtwProvider = "{06714745-69DC-41FF-9684-C532D5183A74}"

$NativeSource = @"
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class ZeroVddBenchNative
{
    private const uint GENERIC_ALL = 0x10000000;
    private const uint WM_QUIT = 0x0012;
    private const uint PM_REMOVE = 0x0001;
    private const uint WS_POPUP = 0x80000000;
    private const uint WS_VISIBLE = 0x10000000;
    private const uint WS_EX_TOPMOST = 0x00000008;
    private const uint WS_EX_TOOLWINDOW = 0x00000080;
    private const uint WS_EX_TRANSPARENT = 0x00000020;
    private const uint WS_EX_NOACTIVATE = 0x08000000;
    private const int SW_SHOWNOACTIVATE = 4;

    private const uint D3D11_SDK_VERSION = 7;
    private const uint D3D_DRIVER_TYPE_UNKNOWN = 0;
    private const uint DXGI_FORMAT_R16G16B16A16_FLOAT = 10;
    private const uint DXGI_FORMAT_R10G10B10A2_UNORM = 24;
    private const uint DXGI_FORMAT_R8G8B8A8_UNORM = 28;
    private const uint DXGI_FORMAT_B8G8R8A8_UNORM = 87;
    private const uint DXGI_USAGE_RENDER_TARGET_OUTPUT = 0x20;
    private const uint DXGI_SWAP_EFFECT_DISCARD = 0;
    private const uint DXGI_SWAP_EFFECT_FLIP_DISCARD = 4;
    private const uint D3D11_USAGE_STAGING = 3;
    private const uint D3D11_CPU_ACCESS_READ = 0x20000;
    private const uint D3D11_MAP_READ = 1;

    private const int DXGI_ERROR_WAIT_TIMEOUT = unchecked((int)0x887A0027);
    private const int DXGI_ERROR_ACCESS_LOST = unchecked((int)0x887A0026);
    private const int DXGI_ERROR_NOT_FOUND = unchecked((int)0x887A0002);
    public const int E_ACCESSDENIED = unchecked((int)0x80070005);

    private const uint TOKEN_DUPLICATE = 0x0002;
    private const uint TOKEN_QUERY = 0x0008;
    private const uint MAXIMUM_ALLOWED = 0x02000000;
    private const uint CREATE_UNICODE_ENVIRONMENT = 0x00000400;
    private const uint CREATE_NO_WINDOW = 0x08000000;

    private static readonly Guid IID_IDXGIFactory1 = new Guid("770aae78-f26f-4dba-a829-253c83d1b387");
    private static readonly Guid IID_IDXGIOutput1 = new Guid("00cddea8-939b-4b83-a340-a685226666cc");
    private static readonly Guid IID_IDXGIOutput5 = new Guid("80A07424-AB52-42EB-833C-0C42FD282D98");
    private static readonly Guid IID_ID3D11Texture2D = new Guid("6f15aaf2-d208-4e89-9ab4-489535d34f9c");

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct POINT
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct WNDCLASSW
    {
        public uint style;
        public IntPtr lpfnWndProc;
        public int cbClsExtra;
        public int cbWndExtra;
        public IntPtr hInstance;
        public IntPtr hIcon;
        public IntPtr hCursor;
        public IntPtr hbrBackground;
        public string lpszMenuName;
        public string lpszClassName;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct MSG
    {
        public IntPtr hwnd;
        public uint message;
        public UIntPtr wParam;
        public IntPtr lParam;
        public uint time;
        public POINT pt;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct DXGI_OUTPUT_DESC
    {
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)]
        public string DeviceName;
        public RECT DesktopCoordinates;
        public int AttachedToDesktop;
        public uint Rotation;
        public IntPtr Monitor;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct DXGI_RATIONAL
    {
        public uint Numerator;
        public uint Denominator;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct DXGI_MODE_DESC
    {
        public uint Width;
        public uint Height;
        public DXGI_RATIONAL RefreshRate;
        public uint Format;
        public uint ScanlineOrdering;
        public uint Scaling;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct DXGI_SAMPLE_DESC
    {
        public uint Count;
        public uint Quality;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct DXGI_SWAP_CHAIN_DESC
    {
        public DXGI_MODE_DESC BufferDesc;
        public DXGI_SAMPLE_DESC SampleDesc;
        public uint BufferUsage;
        public uint BufferCount;
        public IntPtr OutputWindow;
        public int Windowed;
        public uint SwapEffect;
        public uint Flags;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct DXGI_OUTDUPL_POINTER_POSITION
    {
        public POINT Position;
        public int Visible;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct DXGI_OUTDUPL_FRAME_INFO
    {
        public long LastPresentTime;
        public long LastMouseUpdateTime;
        public uint AccumulatedFrames;
        public int RectsCoalesced;
        public int ProtectedContentMaskedOut;
        public DXGI_OUTDUPL_POINTER_POSITION PointerPosition;
        public uint TotalMetadataBufferSize;
        public uint PointerShapeBufferSize;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct DXGI_OUTDUPL_DESC
    {
        public DXGI_MODE_DESC ModeDesc;
        public uint Rotation;
        public int DesktopImageInSystemMemory;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct D3D11_TEXTURE2D_DESC
    {
        public uint Width;
        public uint Height;
        public uint MipLevels;
        public uint ArraySize;
        public uint Format;
        public DXGI_SAMPLE_DESC SampleDesc;
        public uint Usage;
        public uint BindFlags;
        public uint CPUAccessFlags;
        public uint MiscFlags;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct D3D11_MAPPED_SUBRESOURCE
    {
        public IntPtr pData;
        public uint RowPitch;
        public uint DepthPitch;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct DEVMODEW
    {
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)]
        public string dmDeviceName;
        public short dmSpecVersion;
        public short dmDriverVersion;
        public short dmSize;
        public short dmDriverExtra;
        public int dmFields;
        public int dmPositionX;
        public int dmPositionY;
        public int dmDisplayOrientation;
        public int dmDisplayFixedOutput;
        public short dmColor;
        public short dmDuplex;
        public short dmYResolution;
        public short dmTTOption;
        public short dmCollate;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)]
        public string dmFormName;
        public short dmLogPixels;
        public int dmBitsPerPel;
        public int dmPelsWidth;
        public int dmPelsHeight;
        public int dmDisplayFlags;
        public int dmDisplayFrequency;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct STARTUPINFO
    {
        public int cb;
        public string lpReserved;
        public string lpDesktop;
        public string lpTitle;
        public int dwX;
        public int dwY;
        public int dwXSize;
        public int dwYSize;
        public int dwXCountChars;
        public int dwYCountChars;
        public int dwFillAttribute;
        public int dwFlags;
        public short wShowWindow;
        public short cbReserved2;
        public IntPtr lpReserved2;
        public IntPtr hStdInput;
        public IntPtr hStdOutput;
        public IntPtr hStdError;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PROCESS_INFORMATION
    {
        public IntPtr hProcess;
        public IntPtr hThread;
        public int dwProcessId;
        public int dwThreadId;
    }

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate IntPtr WndProcDelegate(IntPtr hwnd, uint msg, UIntPtr wp, IntPtr lp);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int QueryInterfaceFn(IntPtr self, ref Guid riid, out IntPtr ppv);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate uint ReleaseFn(IntPtr self);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int EnumAdapters1Fn(IntPtr self, uint adapterIndex, out IntPtr ppAdapter);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int CreateSwapChainFn(IntPtr self, IntPtr pDevice, ref DXGI_SWAP_CHAIN_DESC pDesc, out IntPtr ppSwapChain);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int EnumOutputsFn(IntPtr self, uint outputIndex, out IntPtr ppOutput);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int OutputGetDescFn(IntPtr self, out DXGI_OUTPUT_DESC pDesc);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int DuplicateOutputFn(IntPtr self, IntPtr pDevice, out IntPtr ppOutputDuplication);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int DuplicateOutput1Fn(
        IntPtr self,
        IntPtr pDevice,
        uint Flags,
        uint SupportedFormatsCount,
        [In] uint[] pSupportedFormats,
        out IntPtr ppOutputDuplication);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate void DuplGetDescFn(IntPtr self, out DXGI_OUTDUPL_DESC pDesc);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int AcquireNextFrameFn(
        IntPtr self,
        uint TimeoutInMilliseconds,
        out DXGI_OUTDUPL_FRAME_INFO pFrameInfo,
        out IntPtr ppDesktopResource);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int ReleaseFrameFn(IntPtr self);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int SwapChainPresentFn(IntPtr self, uint SyncInterval, uint Flags);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int SwapChainGetBufferFn(IntPtr self, uint Buffer, ref Guid riid, out IntPtr ppSurface);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int CreateTexture2DFn(IntPtr self, ref D3D11_TEXTURE2D_DESC pDesc, IntPtr pInitialData, out IntPtr ppTexture2D);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int CreateRenderTargetViewFn(IntPtr self, IntPtr pResource, IntPtr pDesc, out IntPtr ppRTView);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate void Texture2DGetDescFn(IntPtr self, out D3D11_TEXTURE2D_DESC pDesc);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int ContextMapFn(IntPtr self, IntPtr pResource, uint Subresource, uint MapType, uint MapFlags, out D3D11_MAPPED_SUBRESOURCE pMappedResource);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate void ContextUnmapFn(IntPtr self, IntPtr pResource, uint Subresource);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate void ContextCopyResourceFn(IntPtr self, IntPtr pDstResource, IntPtr pSrcResource);

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate void ContextClearRtvFn(IntPtr self, IntPtr pRenderTargetView, [In] float[] ColorRGBA);

    [DllImport("dxgi.dll")]
    private static extern int CreateDXGIFactory1(ref Guid riid, out IntPtr ppFactory);

    [DllImport("d3d11.dll")]
    private static extern int D3D11CreateDevice(
        IntPtr pAdapter,
        uint DriverType,
        IntPtr Software,
        uint Flags,
        [In] uint[] pFeatureLevels,
        uint FeatureLevels,
        uint SDKVersion,
        out IntPtr ppDevice,
        out uint pFeatureLevel,
        out IntPtr ppImmediateContext);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr dpiContext);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr OpenInputDesktop(uint dwFlags, bool fInherit, uint dwDesiredAccess);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetThreadDesktop(IntPtr hDesktop);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool CloseDesktop(IntPtr hDesktop);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern ushort RegisterClassW(ref WNDCLASSW lpWndClass);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool UnregisterClassW(string lpClassName, IntPtr hInstance);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateWindowExW(
        uint dwExStyle,
        string lpClassName,
        string lpWindowName,
        uint dwStyle,
        int x,
        int y,
        int nWidth,
        int nHeight,
        IntPtr hWndParent,
        IntPtr hMenu,
        IntPtr hInstance,
        IntPtr lpParam);

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    private static extern bool DestroyWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern IntPtr DefWindowProcW(IntPtr hWnd, uint msg, UIntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool PeekMessageW(out MSG lpMsg, IntPtr hWnd, uint wMsgFilterMin, uint wMsgFilterMax, uint wRemoveMsg);

    [DllImport("user32.dll")]
    private static extern bool TranslateMessage(ref MSG lpMsg);

    [DllImport("user32.dll")]
    private static extern IntPtr DispatchMessageW(ref MSG lpMsg);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern bool EnumDisplaySettingsW(string lpszDeviceName, int iModeNum, ref DEVMODEW lpDevMode);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr GetModuleHandleW(string lpModuleName);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool OpenProcessToken(IntPtr ProcessHandle, uint DesiredAccess, out IntPtr TokenHandle);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool DuplicateTokenEx(
        IntPtr hExistingToken,
        uint dwDesiredAccess,
        IntPtr lpTokenAttributes,
        int ImpersonationLevel,
        int TokenType,
        out IntPtr phNewToken);

    [DllImport("userenv.dll", SetLastError = true)]
    private static extern bool CreateEnvironmentBlock(out IntPtr lpEnvironment, IntPtr hToken, bool bInherit);

    [DllImport("userenv.dll", SetLastError = true)]
    private static extern bool DestroyEnvironmentBlock(IntPtr lpEnvironment);

    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern bool CreateProcessWithTokenW(
        IntPtr hToken,
        uint dwLogonFlags,
        string lpApplicationName,
        string lpCommandLine,
        uint dwCreationFlags,
        IntPtr lpEnvironment,
        string lpCurrentDirectory,
        ref STARTUPINFO lpStartupInfo,
        out PROCESS_INFORMATION lpProcessInformation);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint WTSGetActiveConsoleSessionId();

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint WaitForSingleObject(IntPtr hHandle, uint dwMilliseconds);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool TerminateProcess(IntPtr hProcess, uint uExitCode);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetExitCodeProcess(IntPtr hProcess, out int lpExitCode);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr hObject);

    private static Delegate GetVtblDelegate(IntPtr obj, int slot, Type delegateType)
    {
        IntPtr vtbl = Marshal.ReadIntPtr(obj);
        IntPtr fnPtr = Marshal.ReadIntPtr(vtbl, slot * IntPtr.Size);
        return Marshal.GetDelegateForFunctionPointer(fnPtr, delegateType);
    }

    private static void SafeRelease(ref IntPtr ptr)
    {
        if (ptr != IntPtr.Zero)
        {
            ReleaseFn release = (ReleaseFn)GetVtblDelegate(ptr, 2, typeof(ReleaseFn));
            release(ptr);
            ptr = IntPtr.Zero;
        }
    }

    public sealed class DistributionStats
    {
        public int Count;
        public double Min;
        public double Mean;
        public double StdDev;
        public double P50;
        public double P95;
        public double P99;
        public double P99_9;
        public double Max;
    }

    public sealed class CaptureBenchResult
    {
        public bool Succeeded;
        public int LastHResult;
        public bool NeedsExplorerFallback;
        public string ErrorMessage;
        public string DisplayName;
        public string AdapterName;
        public int Width;
        public int Height;
        public uint DxgiFormat;
        public double NominalRefreshHz;
        public double WarmupElapsedSeconds;
        public double MeasuredElapsedSeconds;
        public double DxgiReadyMs;
        public double FirstFrameAcquireMs;
        public int CapturedFrames;
        public long CoalescedFrames;
        public int MetaOnlyUpdates;
        public int Timeouts;
        public int AccessLostEvents;
        public int StalledIntervals;
        public double EffectiveFps;
        public long StimulusPresents;
        public long StartNonZeroSamples;
        public long EndNonZeroSamples;
        public DistributionStats PresentIntervalMs;
        public DistributionStats AcquireIntervalMs;
        public DistributionStats CaptureLatencyMs;
    }

    private sealed class StimulusState
    {
        public IntPtr Adapter;
        public RECT TargetRect;
        public volatile bool StopRequested;
        public ManualResetEventSlim ReadyEvent = new ManualResetEventSlim(false);
        public long Presents;
        public string ErrorMessage;
    }

    private static IntPtr SimpleWndProc(IntPtr hwnd, uint msg, UIntPtr wp, IntPtr lp)
    {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }

    private static void AttachThreadToInputDesktop()
    {
        try
        {
            IntPtr prev = SetThreadDpiAwarenessContext(new IntPtr(-4));
            if (prev == IntPtr.Zero)
            {
                SetThreadDpiAwarenessContext(new IntPtr(-3));
            }
        }
        catch
        {
        }

        IntPtr desk = OpenInputDesktop(0, false, GENERIC_ALL);
        if (desk != IntPtr.Zero)
        {
            SetThreadDesktop(desk);
            CloseDesktop(desk);
        }
    }

    private static void RunStimulusThread(object stateObj)
    {
        StimulusState state = (StimulusState)stateObj;
        AttachThreadToInputDesktop();

        string className = "ZeroVddBenchStimulus_" + Guid.NewGuid().ToString("N");
        IntPtr hInstance = GetModuleHandleW(null);
        WndProcDelegate wndProc = new WndProcDelegate(SimpleWndProc);
        IntPtr hwnd = IntPtr.Zero;
        IntPtr factory = IntPtr.Zero;
        IntPtr device = IntPtr.Zero;
        IntPtr context = IntPtr.Zero;
        IntPtr swapChain = IntPtr.Zero;
        IntPtr backBuffer = IntPtr.Zero;
        IntPtr rtv = IntPtr.Zero;

        try
        {
            WNDCLASSW wc = new WNDCLASSW();
            wc.lpfnWndProc = Marshal.GetFunctionPointerForDelegate(wndProc);
            wc.hInstance = hInstance;
            wc.lpszClassName = className;
            if (RegisterClassW(ref wc) == 0)
            {
                state.ErrorMessage = "RegisterClassW failed: " + Marshal.GetLastWin32Error();
                return;
            }

            int wx = state.TargetRect.Left + 8;
            int wy = state.TargetRect.Top + 8;
            hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                className,
                "ZeroVddBenchStimulus",
                WS_POPUP | WS_VISIBLE,
                wx,
                wy,
                64,
                64,
                IntPtr.Zero,
                IntPtr.Zero,
                hInstance,
                IntPtr.Zero);
            if (hwnd == IntPtr.Zero)
            {
                state.ErrorMessage = "CreateWindowExW failed: " + Marshal.GetLastWin32Error();
                return;
            }
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            Guid fid = IID_IDXGIFactory1;
            int hr = CreateDXGIFactory1(ref fid, out factory);
            if (hr < 0)
            {
                state.ErrorMessage = string.Format("Stimulus CreateDXGIFactory1 failed: 0x{0:X8}", hr);
                return;
            }

            uint fl = 0;
            hr = D3D11CreateDevice(state.Adapter, D3D_DRIVER_TYPE_UNKNOWN, IntPtr.Zero, 0, null, 0, D3D11_SDK_VERSION, out device, out fl, out context);
            if (hr < 0)
            {
                state.ErrorMessage = string.Format("Stimulus D3D11CreateDevice failed: 0x{0:X8}", hr);
                return;
            }

            CreateSwapChainFn createSc = (CreateSwapChainFn)GetVtblDelegate(factory, 10, typeof(CreateSwapChainFn));
            DXGI_SWAP_CHAIN_DESC scDesc = new DXGI_SWAP_CHAIN_DESC();
            scDesc.BufferDesc.Width = 64;
            scDesc.BufferDesc.Height = 64;
            scDesc.BufferDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
            scDesc.SampleDesc.Count = 1;
            scDesc.SampleDesc.Quality = 0;
            scDesc.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
            scDesc.BufferCount = 2;
            scDesc.OutputWindow = hwnd;
            scDesc.Windowed = 1;
            scDesc.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;
            hr = createSc(factory, device, ref scDesc, out swapChain);
            if (hr < 0)
            {
                scDesc.SwapEffect = DXGI_SWAP_EFFECT_DISCARD;
                hr = createSc(factory, device, ref scDesc, out swapChain);
            }
            if (hr < 0)
            {
                state.ErrorMessage = string.Format("Stimulus CreateSwapChain failed: 0x{0:X8}", hr);
                return;
            }

            SwapChainGetBufferFn getBuffer = (SwapChainGetBufferFn)GetVtblDelegate(swapChain, 9, typeof(SwapChainGetBufferFn));
            Guid texIid = IID_ID3D11Texture2D;
            hr = getBuffer(swapChain, 0, ref texIid, out backBuffer);
            if (hr < 0)
            {
                state.ErrorMessage = string.Format("Stimulus GetBuffer failed: 0x{0:X8}", hr);
                return;
            }

            CreateRenderTargetViewFn createRtv = (CreateRenderTargetViewFn)GetVtblDelegate(device, 9, typeof(CreateRenderTargetViewFn));
            hr = createRtv(device, backBuffer, IntPtr.Zero, out rtv);
            if (hr < 0)
            {
                state.ErrorMessage = string.Format("Stimulus CreateRenderTargetView failed: 0x{0:X8}", hr);
                return;
            }
            SafeRelease(ref backBuffer);

            ContextClearRtvFn clearRtv = (ContextClearRtvFn)GetVtblDelegate(context, 50, typeof(ContextClearRtvFn));
            SwapChainPresentFn present = (SwapChainPresentFn)GetVtblDelegate(swapChain, 8, typeof(SwapChainPresentFn));

            float[] rgba = new float[4];
            uint frameIdx = 0;
            state.ReadyEvent.Set();

            while (!state.StopRequested)
            {
                MSG msg;
                while (PeekMessageW(out msg, IntPtr.Zero, 0, 0, PM_REMOVE))
                {
                    if (msg.message == WM_QUIT)
                    {
                        state.StopRequested = true;
                        break;
                    }
                    TranslateMessage(ref msg);
                    DispatchMessageW(ref msg);
                }

                frameIdx++;
                rgba[0] = ((frameIdx % 200) + 30) / 255.0f;
                rgba[1] = (((frameIdx * 3) % 200) + 30) / 255.0f;
                rgba[2] = (((frameIdx * 7) % 200) + 30) / 255.0f;
                rgba[3] = 1.0f;

                clearRtv(context, rtv, rgba);
                int phr = present(swapChain, 1, 0);
                if (phr < 0)
                {
                    Thread.Sleep(4);
                }
                else
                {
                    Interlocked.Increment(ref state.Presents);
                }
            }
        }
        catch (Exception ex)
        {
            state.ErrorMessage = ex.Message;
        }
        finally
        {
            state.ReadyEvent.Set();
            SafeRelease(ref rtv);
            SafeRelease(ref backBuffer);
            SafeRelease(ref swapChain);
            SafeRelease(ref context);
            SafeRelease(ref device);
            SafeRelease(ref factory);
            if (hwnd != IntPtr.Zero)
            {
                DestroyWindow(hwnd);
            }
            UnregisterClassW(className, hInstance);
            GC.KeepAlive(wndProc);
        }
    }

    private static int TryCreateDuplication(IntPtr output, IntPtr device, out IntPtr dup)
    {
        dup = IntPtr.Zero;
        QueryInterfaceFn qi = (QueryInterfaceFn)GetVtblDelegate(output, 0, typeof(QueryInterfaceFn));
        IntPtr out5 = IntPtr.Zero;
        Guid iid5 = IID_IDXGIOutput5;
        int hr = unchecked((int)0x80004005);
        if (qi(output, ref iid5, out out5) >= 0 && out5 != IntPtr.Zero)
        {
            try
            {
                DuplicateOutput1Fn dup1 = (DuplicateOutput1Fn)GetVtblDelegate(out5, 26, typeof(DuplicateOutput1Fn));
                uint[] formats = new uint[]
                {
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_FORMAT_R8G8B8A8_UNORM,
                    DXGI_FORMAT_R10G10B10A2_UNORM
                };
                hr = dup1(out5, device, 0, (uint)formats.Length, formats, out dup);
                if (hr >= 0 && dup != IntPtr.Zero)
                {
                    return hr;
                }
            }
            finally
            {
                SafeRelease(ref out5);
            }
        }

        IntPtr out1 = IntPtr.Zero;
        Guid iid1 = IID_IDXGIOutput1;
        int qiHr = qi(output, ref iid1, out out1);
        if (qiHr < 0 || out1 == IntPtr.Zero)
        {
            return hr < 0 ? hr : qiHr;
        }
        try
        {
            DuplicateOutputFn dup0 = (DuplicateOutputFn)GetVtblDelegate(out1, 22, typeof(DuplicateOutputFn));
            return dup0(out1, device, out dup);
        }
        finally
        {
            SafeRelease(ref out1);
        }
    }

    private static long SampleFrameNonZero(
        IntPtr device,
        IntPtr context,
        IntPtr resource,
        ref IntPtr staging,
        ref D3D11_TEXTURE2D_DESC stagingDesc,
        out int width,
        out int height,
        out uint format)
    {
        width = 0;
        height = 0;
        format = 0;
        if (resource == IntPtr.Zero)
        {
            return 0;
        }

        QueryInterfaceFn qi = (QueryInterfaceFn)GetVtblDelegate(resource, 0, typeof(QueryInterfaceFn));
        Guid texIid = IID_ID3D11Texture2D;
        IntPtr tex = IntPtr.Zero;
        if (qi(resource, ref texIid, out tex) < 0 || tex == IntPtr.Zero)
        {
            return 0;
        }

        try
        {
            Texture2DGetDescFn texGetDesc = (Texture2DGetDescFn)GetVtblDelegate(tex, 10, typeof(Texture2DGetDescFn));
            D3D11_TEXTURE2D_DESC td;
            texGetDesc(tex, out td);
            width = (int)td.Width;
            height = (int)td.Height;
            format = td.Format;

            if (staging == IntPtr.Zero || stagingDesc.Width != td.Width || stagingDesc.Height != td.Height || stagingDesc.Format != td.Format)
            {
                SafeRelease(ref staging);
                stagingDesc = td;
                stagingDesc.Usage = D3D11_USAGE_STAGING;
                stagingDesc.BindFlags = 0;
                stagingDesc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
                stagingDesc.MiscFlags = 0;
                CreateTexture2DFn createTex = (CreateTexture2DFn)GetVtblDelegate(device, 5, typeof(CreateTexture2DFn));
                int hr = createTex(device, ref stagingDesc, IntPtr.Zero, out staging);
                if (hr < 0)
                {
                    staging = IntPtr.Zero;
                    return 0;
                }
            }

            ContextCopyResourceFn copyRes = (ContextCopyResourceFn)GetVtblDelegate(context, 47, typeof(ContextCopyResourceFn));
            ContextMapFn mapFn = (ContextMapFn)GetVtblDelegate(context, 14, typeof(ContextMapFn));
            ContextUnmapFn unmapFn = (ContextUnmapFn)GetVtblDelegate(context, 15, typeof(ContextUnmapFn));

            copyRes(context, staging, tex);
            D3D11_MAPPED_SUBRESOURCE mapped;
            if (mapFn(context, staging, 0, D3D11_MAP_READ, 0, out mapped) < 0)
            {
                return 0;
            }

            long nonZero = 0;
            try
            {
                uint bpp = (td.Format == DXGI_FORMAT_R16G16B16A16_FLOAT) ? 8u : 4u;
                uint stepY = td.Height > 64 ? (td.Height / 64) : 1;
                uint stepX = td.Width > 128 ? (td.Width / 128) : 1;
                long basePtr = mapped.pData.ToInt64();
                for (uint y = 0; y < td.Height; y += stepY)
                {
                    long rowAddr = checked(basePtr + (long)y * (long)mapped.RowPitch);
                    for (uint x = 0; x < td.Width; x += stepX)
                    {
                        IntPtr pxPtr = new IntPtr(checked(rowAddr + (long)x * (long)bpp));
                        for (uint b = 0; b < 3 && b < bpp; b++)
                        {
                            if (Marshal.ReadByte(pxPtr, (int)b) != 0)
                            {
                                nonZero++;
                            }
                        }
                    }
                }
                for (uint y = 12; y < 70 && y < td.Height; y += 6)
                {
                    long rowAddr = checked(basePtr + (long)y * (long)mapped.RowPitch);
                    for (uint x = 12; x < 70 && x < td.Width; x += 6)
                    {
                        IntPtr pxPtr = new IntPtr(checked(rowAddr + (long)x * (long)bpp));
                        for (uint b = 0; b < 3 && b < bpp; b++)
                        {
                            if (Marshal.ReadByte(pxPtr, (int)b) != 0)
                            {
                                nonZero++;
                            }
                        }
                    }
                }
            }
            finally
            {
                unmapFn(context, staging, 0);
            }
            return nonZero;
        }
        finally
        {
            SafeRelease(ref tex);
        }
    }

    private static DistributionStats ComputeStats(List<double> samples)
    {
        DistributionStats s = new DistributionStats();
        s.Count = samples.Count;
        if (samples.Count == 0)
        {
            return s;
        }

        samples.Sort();
        double sum = 0.0;
        for (int i = 0; i < samples.Count; i++)
        {
            sum += samples[i];
        }
        double mean = sum / samples.Count;
        double varSum = 0.0;
        for (int i = 0; i < samples.Count; i++)
        {
            double d = samples[i] - mean;
            varSum += d * d;
        }

        s.Min = Math.Round(samples[0], 4);
        s.Mean = Math.Round(mean, 4);
        s.StdDev = Math.Round(Math.Sqrt(varSum / samples.Count), 4);
        s.P50 = Math.Round(Percentile(samples, 0.50), 4);
        s.P95 = Math.Round(Percentile(samples, 0.95), 4);
        s.P99 = Math.Round(Percentile(samples, 0.99), 4);
        s.P99_9 = Math.Round(Percentile(samples, 0.999), 4);
        s.Max = Math.Round(samples[samples.Count - 1], 4);
        return s;
    }

    private static double Percentile(List<double> sorted, double q)
    {
        if (sorted.Count == 0) return 0.0;
        if (sorted.Count == 1) return sorted[0];
        double idx = q * (sorted.Count - 1);
        int lo = (int)Math.Floor(idx);
        int hi = (int)Math.Ceiling(idx);
        if (lo == hi) return sorted[lo];
        double frac = idx - lo;
        return sorted[lo] * (1.0 - frac) + sorted[hi] * frac;
    }

    public static CaptureBenchResult RunCaptureBenchmark(string targetDisplayName, int warmupSeconds, int durationSeconds, bool useStimulus)
    {
        CaptureBenchResult result = new CaptureBenchResult();
        result.DisplayName = targetDisplayName;
        AttachThreadToInputDesktop();

        Stopwatch overallSw = Stopwatch.StartNew();
        IntPtr factory = IntPtr.Zero;
        IntPtr matchedAdapter = IntPtr.Zero;
        IntPtr matchedOutput = IntPtr.Zero;
        IntPtr device = IntPtr.Zero;
        IntPtr context = IntPtr.Zero;
        IntPtr dup = IntPtr.Zero;
        IntPtr staging = IntPtr.Zero;
        D3D11_TEXTURE2D_DESC stagingDesc = new D3D11_TEXTURE2D_DESC();
        StimulusState stimulusState = null;
        Thread stimulusThread = null;

        try
        {
            Guid fid = IID_IDXGIFactory1;
            int hr = CreateDXGIFactory1(ref fid, out factory);
            result.LastHResult = hr;
            if (hr < 0)
            {
                result.ErrorMessage = string.Format("CreateDXGIFactory1 failed: 0x{0:X8}", hr);
                return result;
            }

            EnumAdapters1Fn enumAdapters = (EnumAdapters1Fn)GetVtblDelegate(factory, 12, typeof(EnumAdapters1Fn));
            DXGI_OUTPUT_DESC outDesc = new DXGI_OUTPUT_DESC();

            for (uint ai = 0; ; ai++)
            {
                IntPtr ad = IntPtr.Zero;
                int ahr = enumAdapters(factory, ai, out ad);
                if (ahr == DXGI_ERROR_NOT_FOUND || ahr < 0 || ad == IntPtr.Zero)
                {
                    break;
                }

                EnumOutputsFn enumOutputs = (EnumOutputsFn)GetVtblDelegate(ad, 7, typeof(EnumOutputsFn));
                for (uint oi = 0; ; oi++)
                {
                    IntPtr o = IntPtr.Zero;
                    int ohr = enumOutputs(ad, oi, out o);
                    if (ohr == DXGI_ERROR_NOT_FOUND || ohr < 0 || o == IntPtr.Zero)
                    {
                        break;
                    }

                    OutputGetDescFn getDesc = (OutputGetDescFn)GetVtblDelegate(o, 7, typeof(OutputGetDescFn));
                    DXGI_OUTPUT_DESC d;
                    if (getDesc(o, out d) >= 0 && string.Equals(d.DeviceName, targetDisplayName, StringComparison.OrdinalIgnoreCase))
                    {
                        matchedAdapter = ad;
                        matchedOutput = o;
                        outDesc = d;
                        break;
                    }
                    SafeRelease(ref o);
                }

                if (matchedOutput != IntPtr.Zero)
                {
                    break;
                }
                SafeRelease(ref ad);
            }

            if (matchedOutput == IntPtr.Zero)
            {
                result.ErrorMessage = "DXGI output not found for " + targetDisplayName;
                return result;
            }

            DEVMODEW dm = new DEVMODEW();
            dm.dmSize = (short)Marshal.SizeOf(typeof(DEVMODEW));
            if (EnumDisplaySettingsW(targetDisplayName, -1, ref dm) && dm.dmDisplayFrequency > 0)
            {
                result.NominalRefreshHz = dm.dmDisplayFrequency;
                result.Width = dm.dmPelsWidth;
                result.Height = dm.dmPelsHeight;
            }

            if (useStimulus)
            {
                stimulusState = new StimulusState();
                stimulusState.Adapter = matchedAdapter;
                stimulusState.TargetRect = outDesc.DesktopCoordinates;
                stimulusThread = new Thread(RunStimulusThread);
                stimulusThread.IsBackground = true;
                stimulusThread.Start(stimulusState);
                stimulusState.ReadyEvent.Wait(3000);
            }

            uint fl = 0;
            hr = D3D11CreateDevice(matchedAdapter, D3D_DRIVER_TYPE_UNKNOWN, IntPtr.Zero, 0, null, 0, D3D11_SDK_VERSION, out device, out fl, out context);
            result.LastHResult = hr;
            if (hr < 0)
            {
                result.ErrorMessage = string.Format("D3D11CreateDevice failed: 0x{0:X8}", hr);
                return result;
            }

            hr = TryCreateDuplication(matchedOutput, device, out dup);
            result.LastHResult = hr;
            if (hr < 0 || dup == IntPtr.Zero)
            {
                result.NeedsExplorerFallback = (hr == E_ACCESSDENIED);
                result.ErrorMessage = string.Format("DuplicateOutput failed: 0x{0:X8}", hr);
                return result;
            }

            DuplGetDescFn duplGetDesc = (DuplGetDescFn)GetVtblDelegate(dup, 7, typeof(DuplGetDescFn));
            DXGI_OUTDUPL_DESC dd;
            duplGetDesc(dup, out dd);
            if (dd.ModeDesc.Width > 0)
            {
                result.Width = (int)dd.ModeDesc.Width;
                result.Height = (int)dd.ModeDesc.Height;
                result.DxgiFormat = dd.ModeDesc.Format;
                if (dd.ModeDesc.RefreshRate.Denominator > 0)
                {
                    result.NominalRefreshHz = Math.Round((double)dd.ModeDesc.RefreshRate.Numerator / dd.ModeDesc.RefreshRate.Denominator, 3);
                }
            }
            result.DxgiReadyMs = Math.Round(overallSw.Elapsed.TotalMilliseconds, 3);

            double qpcMsFactor = 1000.0 / Stopwatch.Frequency;
            double nominalPeriodMs = result.NominalRefreshHz > 0.0 ? (1000.0 / result.NominalRefreshHz) : 16.667;
            double stallThresholdMs = nominalPeriodMs * 1.5;

            AcquireNextFrameFn acquireFrame = (AcquireNextFrameFn)GetVtblDelegate(dup, 8, typeof(AcquireNextFrameFn));
            ReleaseFrameFn releaseFrame = (ReleaseFrameFn)GetVtblDelegate(dup, 14, typeof(ReleaseFrameFn));

            long qpcBase = Stopwatch.GetTimestamp();
            long warmupEndQpc = qpcBase + (long)(warmupSeconds * (double)Stopwatch.Frequency);
            bool haveFirstFrame = false;

            while (Stopwatch.GetTimestamp() < warmupEndQpc || !haveFirstFrame)
            {
                if (Stopwatch.GetTimestamp() - qpcBase > (long)((warmupSeconds + 5) * (double)Stopwatch.Frequency))
                {
                    break;
                }
                DXGI_OUTDUPL_FRAME_INFO fi;
                IntPtr res = IntPtr.Zero;
                hr = acquireFrame(dup, 100, out fi, out res);
                if (hr == DXGI_ERROR_WAIT_TIMEOUT)
                {
                    continue;
                }
                if (hr == DXGI_ERROR_ACCESS_LOST)
                {
                    result.AccessLostEvents++;
                    SafeRelease(ref dup);
                    Thread.Sleep(50);
                    hr = TryCreateDuplication(matchedOutput, device, out dup);
                    if (hr < 0 || dup == IntPtr.Zero)
                    {
                        result.LastHResult = hr;
                        result.ErrorMessage = string.Format("DuplicateOutput reacquire failed during warmup: 0x{0:X8}", hr);
                        return result;
                    }
                    acquireFrame = (AcquireNextFrameFn)GetVtblDelegate(dup, 8, typeof(AcquireNextFrameFn));
                    releaseFrame = (ReleaseFrameFn)GetVtblDelegate(dup, 14, typeof(ReleaseFrameFn));
                    continue;
                }
                if (hr < 0)
                {
                    result.LastHResult = hr;
                    result.ErrorMessage = string.Format("AcquireNextFrame failed during warmup: 0x{0:X8}", hr);
                    return result;
                }

                try
                {
                    if (fi.AccumulatedFrames > 0)
                    {
                        if (!haveFirstFrame)
                        {
                            haveFirstFrame = true;
                            result.FirstFrameAcquireMs = Math.Round(overallSw.Elapsed.TotalMilliseconds, 3);
                            int sw, sh;
                            uint sfmt;
                            result.StartNonZeroSamples = SampleFrameNonZero(device, context, res, ref staging, ref stagingDesc, out sw, out sh, out sfmt);
                            if (sw > 0) result.Width = sw;
                            if (sh > 0) result.Height = sh;
                            if (sfmt > 0) result.DxgiFormat = sfmt;
                        }
                    }
                }
                finally
                {
                    SafeRelease(ref res);
                    releaseFrame(dup);
                }
            }
            result.WarmupElapsedSeconds = Math.Round((Stopwatch.GetTimestamp() - qpcBase) * qpcMsFactor / 1000.0, 3);

            int estCap = (int)(Math.Max(1.0, result.NominalRefreshHz) * durationSeconds * 1.2) + 64;
            List<double> presentIntervals = new List<double>(estCap);
            List<double> acquireIntervals = new List<double>(estCap);
            List<double> captureLatencies = new List<double>(estCap);

            long prevPresentQpc = 0;
            long prevAcquireQpc = 0;

            long measStartQpc = Stopwatch.GetTimestamp();
            long measEndQpc = measStartQpc + (long)(durationSeconds * (double)Stopwatch.Frequency);

            while (Stopwatch.GetTimestamp() < measEndQpc)
            {
                DXGI_OUTDUPL_FRAME_INFO fi;
                IntPtr res = IntPtr.Zero;
                hr = acquireFrame(dup, 100, out fi, out res);
                long nowQpc = Stopwatch.GetTimestamp();

                if (hr == DXGI_ERROR_WAIT_TIMEOUT)
                {
                    result.Timeouts++;
                    continue;
                }
                if (hr == DXGI_ERROR_ACCESS_LOST)
                {
                    result.AccessLostEvents++;
                    SafeRelease(ref dup);
                    prevPresentQpc = 0;
                    prevAcquireQpc = 0;
                    Thread.Sleep(50);
                    hr = TryCreateDuplication(matchedOutput, device, out dup);
                    if (hr < 0 || dup == IntPtr.Zero)
                    {
                        result.LastHResult = hr;
                        result.ErrorMessage = string.Format("DuplicateOutput reacquire failed during measurement: 0x{0:X8}", hr);
                        return result;
                    }
                    acquireFrame = (AcquireNextFrameFn)GetVtblDelegate(dup, 8, typeof(AcquireNextFrameFn));
                    releaseFrame = (ReleaseFrameFn)GetVtblDelegate(dup, 14, typeof(ReleaseFrameFn));
                    continue;
                }
                if (hr < 0)
                {
                    result.LastHResult = hr;
                    result.ErrorMessage = string.Format("AcquireNextFrame failed during measurement: 0x{0:X8}", hr);
                    return result;
                }

                try
                {
                    if (fi.AccumulatedFrames == 0)
                    {
                        result.MetaOnlyUpdates++;
                        continue;
                    }

                    result.CapturedFrames++;
                    if (fi.AccumulatedFrames > 1)
                    {
                        result.CoalescedFrames += (long)(fi.AccumulatedFrames - 1);
                    }

                    if (prevAcquireQpc > 0)
                    {
                        acquireIntervals.Add((nowQpc - prevAcquireQpc) * qpcMsFactor);
                    }
                    prevAcquireQpc = nowQpc;

                    if (fi.LastPresentTime > 0)
                    {
                        if (prevPresentQpc > 0 && fi.LastPresentTime > prevPresentQpc)
                        {
                            double pDeltaMs = (fi.LastPresentTime - prevPresentQpc) * qpcMsFactor;
                            presentIntervals.Add(pDeltaMs);
                            if (pDeltaMs > stallThresholdMs)
                            {
                                result.StalledIntervals++;
                            }
                        }
                        prevPresentQpc = fi.LastPresentTime;

                        if (nowQpc >= fi.LastPresentTime)
                        {
                            captureLatencies.Add((nowQpc - fi.LastPresentTime) * qpcMsFactor);
                        }
                    }
                }
                finally
                {
                    SafeRelease(ref res);
                    releaseFrame(dup);
                }
            }

            long measFinishedQpc = Stopwatch.GetTimestamp();
            result.MeasuredElapsedSeconds = Math.Round((measFinishedQpc - measStartQpc) * qpcMsFactor / 1000.0, 4);
            if (result.MeasuredElapsedSeconds > 0.0)
            {
                result.EffectiveFps = Math.Round(result.CapturedFrames / result.MeasuredElapsedSeconds, 2);
            }

            // Final pixel non-zero check outside the timed loop.
            long endDeadline = Stopwatch.GetTimestamp() + (long)(0.8 * Stopwatch.Frequency);
            while (Stopwatch.GetTimestamp() < endDeadline)
            {
                DXGI_OUTDUPL_FRAME_INFO fi;
                IntPtr res = IntPtr.Zero;
                int ehr = acquireFrame(dup, 120, out fi, out res);
                if (ehr == DXGI_ERROR_WAIT_TIMEOUT)
                {
                    continue;
                }
                if (ehr < 0)
                {
                    break;
                }
                try
                {
                    if (fi.AccumulatedFrames > 0)
                    {
                        int ew, eh;
                        uint efmt;
                        result.EndNonZeroSamples = SampleFrameNonZero(device, context, res, ref staging, ref stagingDesc, out ew, out eh, out efmt);
                        break;
                    }
                }
                finally
                {
                    SafeRelease(ref res);
                    releaseFrame(dup);
                }
            }

            result.PresentIntervalMs = ComputeStats(presentIntervals);
            result.AcquireIntervalMs = ComputeStats(acquireIntervals);
            result.CaptureLatencyMs = ComputeStats(captureLatencies);
            result.LastHResult = 0;
            result.Succeeded = true;
        }
        finally
        {
            if (stimulusState != null)
            {
                stimulusState.StopRequested = true;
                if (stimulusThread != null)
                {
                    stimulusThread.Join(2000);
                }
                result.StimulusPresents = stimulusState.Presents;
            }
            SafeRelease(ref staging);
            SafeRelease(ref dup);
            SafeRelease(ref context);
            SafeRelease(ref device);
            SafeRelease(ref matchedOutput);
            SafeRelease(ref matchedAdapter);
            SafeRelease(ref factory);
        }

        return result;
    }

    public static int RunUnderExplorerToken(string exePath, string arguments, int timeoutMs)
    {
        Process[] explorers = Process.GetProcessesByName("explorer");
        if (explorers.Length == 0)
        {
            throw new InvalidOperationException("explorer.exe is not running; cannot obtain interactive user token.");
        }

        IntPtr hTok = IntPtr.Zero;
        IntPtr hDup = IntPtr.Zero;
        IntPtr env = IntPtr.Zero;
        PROCESS_INFORMATION pi = new PROCESS_INFORMATION();

        try
        {
            uint activeSession = WTSGetActiveConsoleSessionId();
            Process targetExplorer = explorers[0];
            for (int i = 0; i < explorers.Length; i++)
            {
                if (activeSession != 0xFFFFFFFFu && (uint)explorers[i].SessionId == activeSession)
                {
                    targetExplorer = explorers[i];
                    break;
                }
            }

            if (!OpenProcessToken(targetExplorer.Handle, TOKEN_DUPLICATE | TOKEN_QUERY, out hTok))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "OpenProcessToken(explorer.exe) failed.");
            }
            if (!DuplicateTokenEx(hTok, MAXIMUM_ALLOWED, IntPtr.Zero, 2, 1, out hDup))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "DuplicateTokenEx failed.");
            }
            if (!CreateEnvironmentBlock(out env, hDup, false))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "CreateEnvironmentBlock failed.");
            }

            STARTUPINFO si = new STARTUPINFO();
            si.cb = Marshal.SizeOf(typeof(STARTUPINFO));
            si.lpDesktop = @"winsta0\default";

            string cmdLine = "\"" + exePath + "\" " + arguments;
            if (!CreateProcessWithTokenW(
                hDup,
                0,
                null,
                cmdLine,
                CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
                env,
                null,
                ref si,
                out pi))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "CreateProcessWithTokenW failed.");
            }

            uint waitRes = WaitForSingleObject(pi.hProcess, (uint)timeoutMs);
            if (waitRes == 0x00000102u)
            {
                TerminateProcess(pi.hProcess, 1u);
                return 124;
            }
            int exitCode = 1;
            GetExitCodeProcess(pi.hProcess, out exitCode);
            return exitCode;
        }
        finally
        {
            if (pi.hThread != IntPtr.Zero) CloseHandle(pi.hThread);
            if (pi.hProcess != IntPtr.Zero) CloseHandle(pi.hProcess);
            if (env != IntPtr.Zero) DestroyEnvironmentBlock(env);
            if (hDup != IntPtr.Zero) CloseHandle(hDup);
            if (hTok != IntPtr.Zero) CloseHandle(hTok);
            for (int i = 0; i < explorers.Length; i++)
            {
                if (explorers[i] != null) explorers[i].Dispose();
            }
        }
    }
}
"@

if (-not ("ZeroVddBenchNative" -as [type])) {
    Add-Type -TypeDefinition $NativeSource -Language CSharp
}

if ($WorkerMode) {
    if ([string]::IsNullOrWhiteSpace($DisplayName)) {
        throw "-WorkerMode requires -DisplayName."
    }
    if ([string]::IsNullOrWhiteSpace($WorkerOutputPath)) {
        throw "-WorkerMode requires -WorkerOutputPath."
    }
    $bench = [ZeroVddBenchNative]::RunCaptureBenchmark($DisplayName, $WarmupSeconds, $DurationSeconds, (-not $NoStimulus))
    $bench | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $WorkerOutputPath -Encoding UTF8
    if ($bench.Succeeded) {
        exit 0
    } else {
        exit 1
    }
}

function Resolve-CliBinary {
    param([string]$PathArg)
    $candidates = @()
    if (-not [string]::IsNullOrWhiteSpace($PathArg)) {
        if ([System.IO.Path]::IsPathRooted($PathArg)) {
            $candidates += $PathArg
        } else {
            $candidates += (Join-Path $RepoRoot $PathArg)
        }
    } else {
        $candidates += (Join-Path $RepoRoot "target\release\zerovdd.exe")
        $candidates += "C:\Program Files\zerovdd\zerovdd.exe"
    }

    foreach ($cand in $candidates) {
        if (Test-Path -LiteralPath $cand -PathType Leaf) {
            return (Resolve-Path -LiteralPath $cand).Path
        }
    }
    throw "zerovdd CLI binary not found. Checked: $($candidates -join ', '). Pass -CliPath explicitly."
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

function Test-IsAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Sample-HostProcesses {
    $wudfInfo = [pscustomobject]@{
        found              = $false
        pid                = $null
        cpu_ms             = 0.0
        working_set_bytes  = [int64]0
        private_bytes      = [int64]0
        handle_count       = 0
    }
    foreach ($p in @(Get-Process -Name "WUDFHost" -ErrorAction SilentlyContinue)) {
        try {
            $hasZeroVdd = $p.Modules | Where-Object { $_.ModuleName -ieq "zerovdd.dll" } | Select-Object -First 1
            if ($hasZeroVdd) {
                $wudfInfo = [pscustomobject]@{
                    found              = $true
                    pid                = $p.Id
                    cpu_ms             = [math]::Round($p.TotalProcessorTime.TotalMilliseconds, 3)
                    working_set_bytes  = [int64]$p.WorkingSet64
                    private_bytes      = [int64]$p.PrivateMemorySize64
                    handle_count       = [int]$p.HandleCount
                }
                break
            }
        } catch {
            # Unelevated callers cannot enumerate modules of LocalService WUDFHost.
        }
    }

    $svcInfo = [pscustomobject]@{
        found              = $false
        pid                = $null
        cpu_ms             = 0.0
        working_set_bytes  = [int64]0
        private_bytes      = [int64]0
        handle_count       = 0
    }
    $svcProc = Get-Process -Name "zerovdd-svc" -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($svcProc) {
        $svcInfo = [pscustomobject]@{
            found              = $true
            pid                = $svcProc.Id
            cpu_ms             = [math]::Round($svcProc.TotalProcessorTime.TotalMilliseconds, 3)
            working_set_bytes  = [int64]$svcProc.WorkingSet64
            private_bytes      = [int64]$svcProc.PrivateMemorySize64
            handle_count       = [int]$svcProc.HandleCount
        }
    }

    return [pscustomobject]@{
        wudf = $wudfInfo
        svc  = $svcInfo
    }
}

function Resolve-ManagedDisplayName {
    param(
        [string[]]$BeforeScreens,
        [int]$TimeoutMs = 8000
    )
    Add-Type -AssemblyName System.Windows.Forms
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        $nowScreens = @([System.Windows.Forms.Screen]::AllScreens | ForEach-Object { $_.DeviceName })
        $added = @($nowScreens | Where-Object { $BeforeScreens -notcontains $_ })
        if ($added.Count -ge 1) {
            return $added[0]
        }
        Start-Sleep -Milliseconds 50
    }
    return ""
}

function Format-StatLine {
    param([string]$Label, $Stats)
    if ($null -eq $Stats -or $Stats.Count -eq 0) {
        return ("  {0,-20} (no samples)" -f $Label)
    }
    return ("  {0,-20} p50={1,7:F3} ms  p95={2,7:F3} ms  p99={3,7:F3} ms  p99.9={4,7:F3} ms  max={5,7:F3} ms  stddev={6,6:F3} ms" -f `
        $Label, [double]$Stats.P50, [double]$Stats.P95, [double]$Stats.P99, [double]$Stats.P99_9, [double]$Stats.Max, [double]$Stats.StdDev)
}

function Invoke-DxgiWorker {
    param(
        [string]$TargetDisplay,
        [bool]$ViaExplorerToken
    )
    if (-not $ViaExplorerToken) {
        return [ZeroVddBenchNative]::RunCaptureBenchmark($TargetDisplay, $WarmupSeconds, $DurationSeconds, (-not $NoStimulus))
    }

    $currentExe = (Get-Process -Id $PID).Path
    $tmpOut = Join-Path $env:TEMP ("zerovdd-bench-worker-" + [guid]::NewGuid().ToString("N") + ".json")
    try {
        $argList = @(
            "-NoProfile",
            "-ExecutionPolicy", "Bypass",
            "-File", "`"$PSCommandPath`"",
            "-WorkerMode",
            "-DisplayName", "`"$TargetDisplay`"",
            "-DurationSeconds", "$DurationSeconds",
            "-WarmupSeconds", "$WarmupSeconds",
            "-WorkerOutputPath", "`"$tmpOut`""
        )
        if ($NoStimulus) {
            $argList += "-NoStimulus"
        }
        $timeoutMs = ($WarmupSeconds + $DurationSeconds + 25) * 1000
        $rc = [ZeroVddBenchNative]::RunUnderExplorerToken($currentExe, ($argList -join " "), $timeoutMs)
        if (-not (Test-Path -LiteralPath $tmpOut -PathType Leaf)) {
            throw "Explorer-token DXGI worker exited ($rc) without producing output at '$tmpOut'."
        }
        return (Get-Content -LiteralPath $tmpOut -Raw | ConvertFrom-Json)
    } finally {
        Remove-Item -LiteralPath $tmpOut -ErrorAction SilentlyContinue
    }
}

$ResolvedCli = Resolve-CliBinary -PathArg $CliPath
$IsAdmin = Test-IsAdministrator

if ($CaptureEtw -and -not $IsAdmin) {
    throw "-CaptureEtw requires an elevated Administrator PowerShell session."
}

$sunshineStatus = Invoke-CliJson -Exe $ResolvedCli -Arguments @("sunshine", "status", "--json") -AllowFailure
$isManaged = [string]::IsNullOrWhiteSpace($DisplayName)

if ($isManaged -and -not $Force) {
    if ($sunshineStatus.ExitCode -eq 0 -and $null -ne $sunshineStatus.Json -and $sunshineStatus.Json.session.active -eq $true) {
        $activeDisp = $sunshineStatus.Json.session.display_name
        throw "Active Sunshine streaming session detected on '$activeDisp'. Refusing to mutate virtual displays without -Force. To benchmark the active stream non-disruptively, re-run with: -DisplayName `"$activeDisp`""
    }
}

$benchStartTime = Get-Date
$createdGuid = $null
$targetDisplay = $DisplayName
$displayAddMs = $null
$displayRemoveMs = $null
$etwSessionName = "ZeroVddBenchEtw_" + [guid]::NewGuid().ToString("N").Substring(0, 8)
$etlPath = Join-Path $env:TEMP ($etwSessionName + ".etl")
$etwStarted = $false
$etwSummary = $null
$benchResult = $null
$hostBefore = $null
$hostAfter = $null

try {
    if ($isManaged) {
        Add-Type -AssemblyName System.Windows.Forms
        $beforeScreens = @([System.Windows.Forms.Screen]::AllScreens | ForEach-Object { $_.DeviceName })
        $addArgs = @("display", "add", $Mode, "--name", "zerovdd-bench", "--lease", ([string]([math]::Max(120, $WarmupSeconds + $DurationSeconds + 60))), "--json")
        if ($Hdr) {
            $addArgs += "--hdr"
        }

        $addSw = [System.Diagnostics.Stopwatch]::StartNew()
        $addRes = Invoke-CliJson -Exe $ResolvedCli -Arguments $addArgs
        $addSw.Stop()
        $displayAddMs = [math]::Round($addSw.Elapsed.TotalMilliseconds, 3)

        $createdGuid = $addRes.Json.monitor_guid
        if ([string]::IsNullOrWhiteSpace($createdGuid)) {
            throw "Managed display add succeeded but returned no monitor_guid."
        }
        $targetDisplay = Resolve-ManagedDisplayName -BeforeScreens $beforeScreens -TimeoutMs 10000
        if ([string]::IsNullOrWhiteSpace($targetDisplay)) {
            throw "Virtual display '$createdGuid' was added, but no new Windows GDI screen became active within 10s."
        }
    }

    if ($CaptureEtw) {
        $logmanOut = & logman create trace $etwSessionName -p $ZeroVddEtwProvider 0xffffffffffffffff 5 -o $etlPath -ets 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw "logman failed to start ETW session '$etwSessionName': $($logmanOut -join ' ')"
        }
        $etwStarted = $true
    }

    $hostBefore = Sample-HostProcesses
    $measureSw = [System.Diagnostics.Stopwatch]::StartNew()

    $benchResult = Invoke-DxgiWorker -TargetDisplay $targetDisplay -ViaExplorerToken:$UseExplorerToken
    if (-not $benchResult.Succeeded -and -not $UseExplorerToken -and $IsAdmin -and $benchResult.NeedsExplorerFallback) {
        Write-Warning "Direct DXGI Desktop Duplication returned E_ACCESSDENIED from elevated session; retrying under explorer.exe interactive token..."
        $benchResult = Invoke-DxgiWorker -TargetDisplay $targetDisplay -ViaExplorerToken:$true
    }

    $measureSw.Stop()
    $hostAfter = Sample-HostProcesses

    if ($etwStarted) {
        $null = & logman stop $etwSessionName -ets 2>&1
        $etwStarted = $false
        if (Test-Path -LiteralPath $etlPath -PathType Leaf) {
            $etlEvents = @(Get-WinEvent -Path $etlPath -Oldest -ErrorAction SilentlyContinue)
            $byLevel = @{}
            $byId = @{}
            foreach ($ev in $etlEvents) {
                $lvl = [string]$ev.LevelDisplayName
                if ([string]::IsNullOrWhiteSpace($lvl)) { $lvl = "Level" + $ev.Level }
                if (-not $byLevel.ContainsKey($lvl)) { $byLevel[$lvl] = 0 }
                $byLevel[$lvl]++

                $idKey = [string]$ev.Id
                if (-not $byId.ContainsKey($idKey)) { $byId[$idKey] = 0 }
                $byId[$idKey]++
            }
            $etwSummary = [pscustomobject]@{
                provider_guid = $ZeroVddEtwProvider
                etl_path      = if ($KeepEtl) { $etlPath } else { $null }
                total_events  = $etlEvents.Count
                by_level      = $byLevel
                by_event_id   = $byId
            }
        }
    }
} finally {
    if ($etwStarted) {
        $null = & logman stop $etwSessionName -ets 2>&1
    }
    if (-not $KeepEtl -and (Test-Path -LiteralPath $etlPath -PathType Leaf)) {
        Remove-Item -LiteralPath $etlPath -ErrorAction SilentlyContinue
    }
    if (-not [string]::IsNullOrWhiteSpace($createdGuid)) {
        $remSw = [System.Diagnostics.Stopwatch]::StartNew()
        $null = Invoke-CliJson -Exe $ResolvedCli -Arguments @("display", "remove", "--guid", $createdGuid, "--json") -AllowFailure
        $remSw.Stop()
        $displayRemoveMs = [math]::Round($remSw.Elapsed.TotalMilliseconds, 3)
    }
}

$wudfCrashEvents = @(Get-WinEvent -FilterHashtable @{
    LogName      = "System"
    ProviderName = "Microsoft-Windows-DriverFrameworks-UserMode"
    StartTime    = $benchStartTime
} -ErrorAction SilentlyContinue | Where-Object { $_.Id -in 10111, 10120, 10121, 4000 })

$wudfPidStable = $true
$wudfMetrics = $null
if ($null -ne $hostBefore -and $null -ne $hostAfter -and $hostBefore.wudf.found) {
    $wudfPidStable = ($hostAfter.wudf.found -and ($hostBefore.wudf.pid -eq $hostAfter.wudf.pid))
    $cpuDeltaMs = [math]::Round(($hostAfter.wudf.cpu_ms - $hostBefore.wudf.cpu_ms), 3)
    $wallMs = [math]::Max(1.0, ($benchResult.WarmupElapsedSeconds + $benchResult.MeasuredElapsedSeconds) * 1000.0)
    $cpuPct1Core = [math]::Round(($cpuDeltaMs / $wallMs) * 100.0, 2)
    $cpuPctSystem = [math]::Round(($cpuPct1Core / [math]::Max(1, [Environment]::ProcessorCount)), 2)
    $wudfMetrics = [pscustomobject]@{
        pid                 = $hostBefore.wudf.pid
        pid_stable          = $wudfPidStable
        cpu_ms              = $cpuDeltaMs
        cpu_pct_1core       = $cpuPct1Core
        cpu_pct_system      = $cpuPctSystem
        private_before_mb   = [math]::Round($hostBefore.wudf.private_bytes / 1MB, 2)
        private_after_mb    = [math]::Round($hostAfter.wudf.private_bytes / 1MB, 2)
        private_delta_mb    = [math]::Round(($hostAfter.wudf.private_bytes - $hostBefore.wudf.private_bytes) / 1MB, 2)
        working_set_mb      = [math]::Round($hostAfter.wudf.working_set_bytes / 1MB, 2)
        handles_before      = $hostBefore.wudf.handle_count
        handles_after       = $hostAfter.wudf.handle_count
        handles_delta       = ($hostAfter.wudf.handle_count - $hostBefore.wudf.handle_count)
    }
}

$svcMetrics = $null
if ($null -ne $hostBefore -and $null -ne $hostAfter -and $hostBefore.svc.found -and $hostAfter.svc.found) {
    $svcCpuDeltaMs = [math]::Round(($hostAfter.svc.cpu_ms - $hostBefore.svc.cpu_ms), 3)
    $svcMetrics = [pscustomobject]@{
        pid               = $hostBefore.svc.pid
        cpu_ms            = $svcCpuDeltaMs
        private_before_mb = [math]::Round($hostBefore.svc.private_bytes / 1MB, 2)
        private_after_mb  = [math]::Round($hostAfter.svc.private_bytes / 1MB, 2)
        working_set_mb    = [math]::Round($hostAfter.svc.working_set_bytes / 1MB, 2)
        handles_before    = $hostBefore.svc.handle_count
        handles_after     = $hostAfter.svc.handle_count
        handles_delta     = ($hostAfter.svc.handle_count - $hostBefore.svc.handle_count)
    }
}

$report = [pscustomobject]@{
    timestamp_utc      = (Get-Date).ToUniversalTime().ToString("o")
    mode_kind          = if ($isManaged) { "managed" } else { "attach" }
    cli_path           = $ResolvedCli
    display_name       = $targetDisplay
    requested_mode     = if ($isManaged) { $Mode } else { $null }
    hdr_requested      = if ($isManaged) { [bool]$Hdr } else { $null }
    stimulus_enabled   = (-not $NoStimulus)
    warmup_seconds     = $WarmupSeconds
    duration_seconds   = $DurationSeconds
    lifecycle_ms       = [pscustomobject]@{
        display_add         = $displayAddMs
        dxgi_ready          = if ($benchResult) { $benchResult.DxgiReadyMs } else { $null }
        first_frame_acquire = if ($benchResult) { $benchResult.FirstFrameAcquireMs } else { $null }
        display_remove      = $displayRemoveMs
    }
    dxgi               = $benchResult
    wudf_host          = $wudfMetrics
    zerovdd_svc       = $svcMetrics
    wudf_crash_events  = $wudfCrashEvents.Count
    etw                = $etwSummary
}

if (-not [string]::IsNullOrWhiteSpace($JsonReport)) {
    $reportDir = Split-Path -Path $JsonReport -Parent
    if (-not [string]::IsNullOrWhiteSpace($reportDir) -and -not (Test-Path -LiteralPath $reportDir)) {
        New-Item -ItemType Directory -Path $reportDir -Force | Out-Null
    }
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $JsonReport -Encoding UTF8
}

Write-Host "================================================================="
Write-Host ("zerovdd Benchmark ({0} mode) on {1}" -f $report.mode_kind, $targetDisplay)
Write-Host "================================================================="
if ($null -eq $benchResult -or -not $benchResult.Succeeded) {
    $errText = if ($benchResult) { $benchResult.ErrorMessage } else { "No capture result produced." }
    throw "Benchmark capture failed: $errText"
}

Write-Host ("  Resolution / Format : {0}x{1} @ {2} Hz (DXGI_FORMAT={3})" -f `
    $benchResult.Width, $benchResult.Height, $benchResult.NominalRefreshHz, $benchResult.DxgiFormat)
Write-Host ("  Window              : warmup={0:F2}s  measured={1:F3}s  stimulus={2}" -f `
    $benchResult.WarmupElapsedSeconds, $benchResult.MeasuredElapsedSeconds, (-not $NoStimulus))
Write-Host ("  Frames              : captured={0}  effective_fps={1:F2}  stimulus_presents={2}  coalesced={3}  meta_only={4}  timeouts={5}  stalls(>1.5x)={6}" -f `
    $benchResult.CapturedFrames, $benchResult.EffectiveFps, $benchResult.StimulusPresents, `
    $benchResult.CoalescedFrames, $benchResult.MetaOnlyUpdates, $benchResult.Timeouts, $benchResult.StalledIntervals)
Write-Host ("  Pixel Verification  : start_non_zero={0}  end_non_zero={1}  access_lost={2}" -f `
    $benchResult.StartNonZeroSamples, $benchResult.EndNonZeroSamples, $benchResult.AccessLostEvents)
Write-Host ("  Startup Latency     : dxgi_ready={0:F2} ms  first_frame={1:F2} ms  add={2} ms  remove={3} ms" -f `
    $benchResult.DxgiReadyMs, $benchResult.FirstFrameAcquireMs, `
    $(if ($null -ne $displayAddMs) { "{0:F2}" -f $displayAddMs } else { "n/a" }), `
    $(if ($null -ne $displayRemoveMs) { "{0:F2}" -f $displayRemoveMs } else { "n/a" }))
Write-Host ""
Write-Host (Format-StatLine -Label "Present interval" -Stats $benchResult.PresentIntervalMs)
Write-Host (Format-StatLine -Label "Acquire interval" -Stats $benchResult.AcquireIntervalMs)
Write-Host (Format-StatLine -Label "Capture latency"  -Stats $benchResult.CaptureLatencyMs)

if ($null -ne $wudfMetrics) {
    Write-Host ""
    Write-Host ("  WUDFHost (PID={0}) : stable={1}  cpu={2:F1} ms ({3:F2}% 1-core, {4:F2}% sys)  priv={5}->{6} MB (delta {7:+#0.00;-#0.00;0.00})  handles={8}->{9} (delta {10:+#;-#;0})" -f `
        $wudfMetrics.pid, $wudfMetrics.pid_stable, $wudfMetrics.cpu_ms, $wudfMetrics.cpu_pct_1core, $wudfMetrics.cpu_pct_system, `
        $wudfMetrics.private_before_mb, $wudfMetrics.private_after_mb, $wudfMetrics.private_delta_mb, `
        $wudfMetrics.handles_before, $wudfMetrics.handles_after, $wudfMetrics.handles_delta)
}
if ($null -ne $svcMetrics) {
    Write-Host ("  zerovdd-svc ({0}) : cpu={1:F1} ms  priv={2}->{3} MB  ws={4} MB  handles={5}->{6}" -f `
        $svcMetrics.pid, $svcMetrics.cpu_ms, $svcMetrics.private_before_mb, $svcMetrics.private_after_mb, `
        $svcMetrics.working_set_mb, $svcMetrics.handles_before, $svcMetrics.handles_after)
}
Write-Host ("  WUDF Crash Events   : {0}" -f $wudfCrashEvents.Count)
if ($null -ne $etwSummary) {
    $lvlPairs = ($etwSummary.by_level.GetEnumerator() | Sort-Object Name | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ", "
    Write-Host ("  ETW (ZeroVdd.Driver): total={0}  levels=[{1}]" -f $etwSummary.total_events, $lvlPairs)
}

if ($wudfCrashEvents.Count -gt 0) {
    throw "Detected $($wudfCrashEvents.Count) WUDFHost crash/restart event(s) during benchmark."
}
if (-not $wudfPidStable) {
    throw "WUDFHost PID changed during benchmark."
}
if (-not $NoStimulus -and ($benchResult.CapturedFrames -le 0 -or $benchResult.StartNonZeroSamples -le 0 -or $benchResult.EndNonZeroSamples -le 0)) {
    throw "Benchmark failed frame/pixel delivery assertions (captured=$($benchResult.CapturedFrames), startNonZero=$($benchResult.StartNonZeroSamples), endNonZero=$($benchResult.EndNonZeroSamples))."
}
