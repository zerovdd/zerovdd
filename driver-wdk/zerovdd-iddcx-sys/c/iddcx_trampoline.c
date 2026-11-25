#define WIN32_LEAN_AND_MEAN
#include <windows.h>

// WHY: WDK UMDF wdfdevice.h forward-declares `enum _WDF_REQUEST_TYPE : int` only under
// __cplusplus, which Clang C++ rejects when wdfrequest.h defines the enum without `: int`.
// Parsing wdf.h in C mode inside extern "C" avoids the mismatch while IddCx.h compiles as C++.
extern "C" {
#pragma push_macro("__cplusplus")
#undef __cplusplus
#include <wdf.h>
#pragma pop_macro("__cplusplus")
}

#undef WDF_EXTERN_C
#undef WDF_EXTERN_C_START
#undef WDF_EXTERN_C_END

#define IDDCX_VERSION_MAJOR 1
#define IDDCX_VERSION_MINOR 10
#define IDDCX_MINIMUM_VERSION_REQUIRED 9
#include <iddcx/1.10/IddCx.h>

#include "iddcx_trampoline.h"

#ifndef D3DKMDT_VSS_OTHER
#define D3DKMDT_VSS_OTHER 255u
#endif

#ifndef STATUS_BUFFER_TOO_SMALL
#define STATUS_BUFFER_TOO_SMALL ((NTSTATUS)0xC0000023L)
#endif

#ifndef STATUS_NO_DATA_DETECTED
#define STATUS_NO_DATA_DETECTED ((NTSTATUS)0x80000022L)
#endif

#ifndef STATUS_NOT_SUPPORTED
#define STATUS_NOT_SUPPORTED ((NTSTATUS)0xC00000BBL)
#endif

#ifndef STATUS_INSUFFICIENT_RESOURCES
#define STATUS_INSUFFICIENT_RESOURCES ((NTSTATUS)0xC000009AL)
#endif

#define ZEROVDD_STACK_MODE_SCRATCH 64u

// WHY: Per-adapter context attached to IDDCX_ADAPTER via IDARG_IN_ADAPTER_INIT::ObjectAttributes
// so EvtIddCxAdapterInitFinished and EvtIddCxAdapterQueryTargetInfo resolve WDFDEVICE without globals.
typedef struct _ZEROVDD_ADAPTER_CTX {
    PVOID volatile Device;
    volatile LONG InitFinished;
    volatile LONG ReadyDispatched;
    volatile NTSTATUS InitStatus;
    zerovdd_evt_adapter_ready_t ReadyCallback;
} ZEROVDD_ADAPTER_CTX, *PZEROVDD_ADAPTER_CTX;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(ZEROVDD_ADAPTER_CTX, ZeroVddGetAdapterContext)

// WHY: Per-monitor context attached to IDDCX_MONITOR via IDARG_IN_MONITORCREATE::ObjectAttributes
// so monitor callbacks resolve the owning WDFDEVICE without mutable global state.
typedef struct _ZEROVDD_MONITOR_CTX {
    PVOID Device;
    UINT ConnectorIndex;
} ZEROVDD_MONITOR_CTX, *PZEROVDD_MONITOR_CTX;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(ZEROVDD_MONITOR_CTX, ZeroVddGetMonitorContext)

// WHY: Store only immutable Rust callback function pointers registered once during EvtDeviceAdd.
static zerovdd_iddcx_callbacks_t g_callbacks = {0};

static void fill_video_signal_info(
    DISPLAYCONFIG_VIDEO_SIGNAL_INFO* dst,
    const zerovdd_video_signal_t* src
) {
    RtlZeroMemory(dst, sizeof(*dst));
    dst->pixelRate = src->pixel_rate;
    dst->hSyncFreq.Numerator = src->hsync_freq_num;
    dst->hSyncFreq.Denominator = src->hsync_freq_den;
    dst->vSyncFreq.Numerator = src->vsync_freq_num;
    dst->vSyncFreq.Denominator = src->vsync_freq_den;
    dst->activeSize.cx = src->active_width;
    dst->activeSize.cy = src->active_height;
    dst->totalSize.cx = src->total_width;
    dst->totalSize.cy = src->total_height;
    dst->AdditionalSignalInfo.videoStandard = D3DKMDT_VSS_OTHER;
    dst->AdditionalSignalInfo.vSyncFreqDivider = src->vsync_freq_divider;
    dst->scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
}

static IDDCX_BITS_PER_COMPONENT map_bpc(uint32_t mask) {
    UINT bpc = IDDCX_BITS_PER_COMPONENT_NONE;
    if ((mask & ZEROVDD_BPC_8) != 0u) {
        bpc |= IDDCX_BITS_PER_COMPONENT_8;
    }
    if ((mask & ZEROVDD_BPC_10) != 0u) {
        bpc |= IDDCX_BITS_PER_COMPONENT_10;
    }
    if ((mask & ZEROVDD_BPC_12) != 0u) {
        bpc |= IDDCX_BITS_PER_COMPONENT_12;
    }
    if (bpc == IDDCX_BITS_PER_COMPONENT_NONE) {
        bpc = IDDCX_BITS_PER_COMPONENT_8;
    }
    return (IDDCX_BITS_PER_COMPONENT)bpc;
}

static uint32_t unmap_bpc(IDDCX_BITS_PER_COMPONENT bpc) {
    uint32_t mask = ZEROVDD_BPC_NONE;
    if ((bpc & IDDCX_BITS_PER_COMPONENT_8) != 0) {
        mask |= ZEROVDD_BPC_8;
    }
    if ((bpc & IDDCX_BITS_PER_COMPONENT_10) != 0) {
        mask |= ZEROVDD_BPC_10;
    }
    if ((bpc & IDDCX_BITS_PER_COMPONENT_12) != 0) {
        mask |= ZEROVDD_BPC_12;
    }
    return mask;
}

static zerovdd_video_signal_t* alloc_scratch_signals(
    uint32_t count,
    zerovdd_video_signal_t* stack_buf,
    bool* out_heap
) {
    *out_heap = false;
    if (count <= ZEROVDD_STACK_MODE_SCRATCH) {
        return stack_buf;
    }
    HANDLE heap = GetProcessHeap();
    if (heap == NULL) {
        return NULL;
    }
    SIZE_T bytes = (SIZE_T)count * sizeof(zerovdd_video_signal_t);
    zerovdd_video_signal_t* ptr = (zerovdd_video_signal_t*)HeapAlloc(heap, HEAP_ZERO_MEMORY, bytes);
    if (ptr != NULL) {
        *out_heap = true;
    }
    return ptr;
}

static void free_scratch_signals(zerovdd_video_signal_t* ptr, bool is_heap) {
    if (is_heap && ptr != NULL) {
        HANDLE heap = GetProcessHeap();
        if (heap != NULL) {
            HeapFree(heap, 0, ptr);
        }
    }
}

static NTSTATUS NTAPI zerovdd_stub_adapter_init_finished(
    IDDCX_ADAPTER AdapterObject,
    const IDARG_IN_ADAPTER_INIT_FINISHED* pInArgs
) {
    if (pInArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    PZEROVDD_ADAPTER_CTX ctx = ZeroVddGetAdapterContext(AdapterObject);
    if (ctx != NULL) {
        InterlockedExchange(&ctx->InitStatus, pInArgs->AdapterInitStatus);
        InterlockedExchange(&ctx->InitFinished, 1);
        PVOID dev = InterlockedCompareExchangePointer(&ctx->Device, NULL, NULL);
        if (dev != NULL &&
            ctx->ReadyCallback != NULL &&
            InterlockedCompareExchange(&ctx->ReadyDispatched, 1, 0) == 0) {
            ctx->ReadyCallback(
                (zerovdd_wdf_device_t)dev,
                (zerovdd_ntstatus_t)pInArgs->AdapterInitStatus
            );
        }
    }
    return STATUS_SUCCESS;
}

static NTSTATUS handle_parse_monitor_description_common(
    const IDDCX_MONITOR_DESCRIPTION* desc,
    UINT input_count,
    PVOID out_modes_raw,
    UINT* out_count_ptr,
    UINT* out_preferred_idx_ptr,
    bool is_v2
) {
    if (desc == NULL || out_count_ptr == NULL || out_preferred_idx_ptr == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *out_count_ptr = 0;
    *out_preferred_idx_ptr = NO_PREFERRED_MODE;

    if (g_callbacks.evt_parse_monitor_description == NULL) {
        return STATUS_NOT_SUPPORTED;
    }
    if (desc->DataSize > 0 && desc->pData == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    uint32_t required_count = 0;
    uint32_t preferred_idx = NO_PREFERRED_MODE;
    NTSTATUS status = (NTSTATUS)g_callbacks.evt_parse_monitor_description(
        (const uint8_t*)desc->pData,
        (uint32_t)desc->DataSize,
        0u,
        NULL,
        &required_count,
        &preferred_idx
    );
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (required_count == 0u) {
        return STATUS_NO_DATA_DETECTED;
    }

    *out_count_ptr = required_count;
    *out_preferred_idx_ptr = preferred_idx;

    if (out_modes_raw == NULL) {
        return (input_count == 0u) ? STATUS_SUCCESS : STATUS_INVALID_PARAMETER;
    }
    if (input_count < required_count) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    zerovdd_video_signal_t stack_buf[ZEROVDD_STACK_MODE_SCRATCH];
    bool is_heap = false;
    zerovdd_video_signal_t* scratch = alloc_scratch_signals(required_count, stack_buf, &is_heap);
    if (scratch == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    uint32_t written_count = 0;
    uint32_t final_preferred_idx = preferred_idx;
    status = (NTSTATUS)g_callbacks.evt_parse_monitor_description(
        (const uint8_t*)desc->pData,
        (uint32_t)desc->DataSize,
        required_count,
        scratch,
        &written_count,
        &final_preferred_idx
    );
    if (!NT_SUCCESS(status)) {
        free_scratch_signals(scratch, is_heap);
        return status;
    }
    if (written_count == 0u) {
        free_scratch_signals(scratch, is_heap);
        return STATUS_NO_DATA_DETECTED;
    }
    if (written_count > input_count) {
        *out_count_ptr = written_count;
        *out_preferred_idx_ptr = final_preferred_idx;
        free_scratch_signals(scratch, is_heap);
        return STATUS_BUFFER_TOO_SMALL;
    }

    if (is_v2) {
        IDDCX_MONITOR_MODE2* dst = (IDDCX_MONITOR_MODE2*)out_modes_raw;
        ULONG struct_size = IDD_STRUCTURE_SIZE(IDDCX_MONITOR_MODE2);
        for (uint32_t i = 0; i < written_count; ++i) {
            RtlZeroMemory(&dst[i], sizeof(IDDCX_MONITOR_MODE2));
            dst[i].Size = struct_size;
            dst[i].Origin = IDDCX_MONITOR_MODE_ORIGIN_MONITORDESCRIPTOR;
            fill_video_signal_info(&dst[i].MonitorVideoSignalInfo, &scratch[i]);
            dst[i].BitsPerComponent.Rgb = map_bpc(scratch[i].bits_per_component_rgb);
        }
    } else {
        IDDCX_MONITOR_MODE* dst = (IDDCX_MONITOR_MODE*)out_modes_raw;
        ULONG struct_size = IDD_STRUCTURE_SIZE(IDDCX_MONITOR_MODE);
        for (uint32_t i = 0; i < written_count; ++i) {
            RtlZeroMemory(&dst[i], sizeof(IDDCX_MONITOR_MODE));
            dst[i].Size = struct_size;
            dst[i].Origin = IDDCX_MONITOR_MODE_ORIGIN_MONITORDESCRIPTOR;
            fill_video_signal_info(&dst[i].MonitorVideoSignalInfo, &scratch[i]);
        }
    }

    *out_count_ptr = written_count;
    *out_preferred_idx_ptr = final_preferred_idx;
    free_scratch_signals(scratch, is_heap);
    return STATUS_SUCCESS;
}

static NTSTATUS NTAPI zerovdd_stub_parse_monitor_description(
    const IDARG_IN_PARSEMONITORDESCRIPTION* pInArgs,
    IDARG_OUT_PARSEMONITORDESCRIPTION* pOutArgs
) {
    if (pInArgs == NULL || pOutArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    return handle_parse_monitor_description_common(
        &pInArgs->MonitorDescription,
        pInArgs->MonitorModeBufferInputCount,
        pInArgs->pMonitorModes,
        &pOutArgs->MonitorModeBufferOutputCount,
        &pOutArgs->PreferredMonitorModeIdx,
        false
    );
}

static NTSTATUS NTAPI zerovdd_stub_parse_monitor_description2(
    const IDARG_IN_PARSEMONITORDESCRIPTION2* pInArgs,
    IDARG_OUT_PARSEMONITORDESCRIPTION* pOutArgs
) {
    if (pInArgs == NULL || pOutArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    return handle_parse_monitor_description_common(
        &pInArgs->MonitorDescription,
        pInArgs->MonitorModeBufferInputCount,
        pInArgs->pMonitorModes,
        &pOutArgs->MonitorModeBufferOutputCount,
        &pOutArgs->PreferredMonitorModeIdx,
        true
    );
}

static NTSTATUS NTAPI zerovdd_stub_adapter_query_target_info(
    IDDCX_ADAPTER AdapterObject,
    IDARG_IN_QUERYTARGET_INFO* pInArgs,
    IDARG_OUT_QUERYTARGET_INFO* pOutArgs
) {
    if (pInArgs == NULL || pOutArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    RtlZeroMemory(pOutArgs, sizeof(*pOutArgs));
    if (g_callbacks.evt_query_target_info == NULL) {
        return STATUS_SUCCESS;
    }

    PZEROVDD_ADAPTER_CTX ctx = ZeroVddGetAdapterContext(AdapterObject);
    PVOID dev = (ctx != NULL) ? InterlockedCompareExchangePointer(&ctx->Device, NULL, NULL) : NULL;
    zerovdd_target_info_t info = {0};
    NTSTATUS status = (NTSTATUS)g_callbacks.evt_query_target_info(
        (zerovdd_wdf_device_t)dev,
        (uint32_t)pInArgs->ConnectorIndex,
        &info
    );
    if (!NT_SUCCESS(status)) {
        return status;
    }

    UINT target_caps = IDDCX_TARGET_CAPS_NONE;
    if ((info.target_caps & ZEROVDD_TARGET_CAPS_WIDE_COLOR_SPACE) != 0u) {
        target_caps |= IDDCX_TARGET_CAPS_WIDE_COLOR_SPACE;
    }
    if ((info.target_caps & ZEROVDD_TARGET_CAPS_HIGH_COLOR_SPACE) != 0u) {
        target_caps |= IDDCX_TARGET_CAPS_HIGH_COLOR_SPACE;
    }
    pOutArgs->TargetCaps = (IDDCX_TARGET_CAPS)target_caps;
    if (info.dithering_rgb != ZEROVDD_BPC_NONE) {
        pOutArgs->DitheringSupport.Rgb = map_bpc(info.dithering_rgb);
    }
    return STATUS_SUCCESS;
}

static NTSTATUS NTAPI zerovdd_stub_adapter_commit_modes(
    IDDCX_ADAPTER AdapterObject,
    const IDARG_IN_COMMITMODES* pInArgs
) {
    if (pInArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (pInArgs->PathCount > 0u && pInArgs->pPaths == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (g_callbacks.evt_commit_modes == NULL) {
        return STATUS_SUCCESS;
    }

    PZEROVDD_ADAPTER_CTX ctx = ZeroVddGetAdapterContext(AdapterObject);
    PVOID dev = (ctx != NULL) ? InterlockedCompareExchangePointer(&ctx->Device, NULL, NULL) : NULL;

    zerovdd_commit_path_t stack_paths[16];
    uint32_t count = (uint32_t)pInArgs->PathCount;
    if (count > 16u) {
        count = 16u;
    }
    for (uint32_t i = 0; i < count; ++i) {
        const IDDCX_PATH* p = &pInArgs->pPaths[i];
        stack_paths[i].monitor = (zerovdd_iddcx_monitor_t)p->MonitorObject;
        stack_paths[i].flags = (uint32_t)p->Flags;
        stack_paths[i].active_width = p->TargetVideoSignalInfo.activeSize.cx;
        stack_paths[i].active_height = p->TargetVideoSignalInfo.activeSize.cy;
        stack_paths[i].vsync_num = p->TargetVideoSignalInfo.vSyncFreq.Numerator;
        stack_paths[i].vsync_den = p->TargetVideoSignalInfo.vSyncFreq.Denominator;
        stack_paths[i].color_space = (uint32_t)IDDCX_COLOR_SPACE_G22_P709;
        stack_paths[i].bits_per_component_rgb = ZEROVDD_BPC_8;
    }

    return (NTSTATUS)g_callbacks.evt_commit_modes(
        (zerovdd_wdf_device_t)dev,
        count,
        stack_paths
    );
}

static NTSTATUS NTAPI zerovdd_stub_adapter_commit_modes2(
    IDDCX_ADAPTER AdapterObject,
    const IDARG_IN_COMMITMODES2* pInArgs
) {
    if (pInArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (pInArgs->PathCount > 0u && pInArgs->pPaths == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (g_callbacks.evt_commit_modes == NULL) {
        return STATUS_SUCCESS;
    }

    PZEROVDD_ADAPTER_CTX ctx = ZeroVddGetAdapterContext(AdapterObject);
    PVOID dev = (ctx != NULL) ? InterlockedCompareExchangePointer(&ctx->Device, NULL, NULL) : NULL;

    zerovdd_commit_path_t stack_paths[16];
    uint32_t count = (uint32_t)pInArgs->PathCount;
    if (count > 16u) {
        count = 16u;
    }
    for (uint32_t i = 0; i < count; ++i) {
        const IDDCX_PATH2* p = &pInArgs->pPaths[i];
        stack_paths[i].monitor = (zerovdd_iddcx_monitor_t)p->MonitorObject;
        stack_paths[i].flags = (uint32_t)p->Flags;
        stack_paths[i].active_width = p->TargetVideoSignalInfo.activeSize.cx;
        stack_paths[i].active_height = p->TargetVideoSignalInfo.activeSize.cy;
        stack_paths[i].vsync_num = p->TargetVideoSignalInfo.vSyncFreq.Numerator;
        stack_paths[i].vsync_den = p->TargetVideoSignalInfo.vSyncFreq.Denominator;
        stack_paths[i].color_space = (uint32_t)p->WireFormatInfo.ColorSpace;
        stack_paths[i].bits_per_component_rgb = unmap_bpc(p->WireFormatInfo.BitsPerComponent.Rgb);
    }

    return (NTSTATUS)g_callbacks.evt_commit_modes(
        (zerovdd_wdf_device_t)dev,
        count,
        stack_paths
    );
}

static NTSTATUS NTAPI zerovdd_stub_monitor_get_default_modes(
    IDDCX_MONITOR MonitorObject,
    const IDARG_IN_GETDEFAULTDESCRIPTIONMODES* pInArgs,
    IDARG_OUT_GETDEFAULTDESCRIPTIONMODES* pOutArgs
) {
    if (pInArgs == NULL || pOutArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    pOutArgs->DefaultMonitorModeBufferOutputCount = 0;
    pOutArgs->PreferredMonitorModeIdx = NO_PREFERRED_MODE;

    if (g_callbacks.evt_get_default_description_modes == NULL) {
        return STATUS_NOT_SUPPORTED;
    }

    PZEROVDD_MONITOR_CTX mctx = ZeroVddGetMonitorContext(MonitorObject);
    PVOID dev = (mctx != NULL) ? mctx->Device : NULL;

    uint32_t required_count = 0;
    uint32_t preferred_idx = NO_PREFERRED_MODE;
    NTSTATUS status = (NTSTATUS)g_callbacks.evt_get_default_description_modes(
        (zerovdd_wdf_device_t)dev,
        (zerovdd_iddcx_monitor_t)MonitorObject,
        0u,
        NULL,
        &required_count,
        &preferred_idx
    );
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (required_count == 0u) {
        return STATUS_NO_DATA_DETECTED;
    }

    pOutArgs->DefaultMonitorModeBufferOutputCount = required_count;
    pOutArgs->PreferredMonitorModeIdx = preferred_idx;

    if (pInArgs->pDefaultMonitorModes == NULL) {
        return (pInArgs->DefaultMonitorModeBufferInputCount == 0u)
            ? STATUS_SUCCESS
            : STATUS_INVALID_PARAMETER;
    }
    if (pInArgs->DefaultMonitorModeBufferInputCount < required_count) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    zerovdd_video_signal_t stack_buf[ZEROVDD_STACK_MODE_SCRATCH];
    bool is_heap = false;
    zerovdd_video_signal_t* scratch = alloc_scratch_signals(required_count, stack_buf, &is_heap);
    if (scratch == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    uint32_t written_count = 0;
    uint32_t final_preferred_idx = preferred_idx;
    status = (NTSTATUS)g_callbacks.evt_get_default_description_modes(
        (zerovdd_wdf_device_t)dev,
        (zerovdd_iddcx_monitor_t)MonitorObject,
        required_count,
        scratch,
        &written_count,
        &final_preferred_idx
    );
    if (!NT_SUCCESS(status)) {
        free_scratch_signals(scratch, is_heap);
        return status;
    }
    if (written_count == 0u) {
        free_scratch_signals(scratch, is_heap);
        return STATUS_NO_DATA_DETECTED;
    }
    if (written_count > pInArgs->DefaultMonitorModeBufferInputCount) {
        pOutArgs->DefaultMonitorModeBufferOutputCount = written_count;
        pOutArgs->PreferredMonitorModeIdx = final_preferred_idx;
        free_scratch_signals(scratch, is_heap);
        return STATUS_BUFFER_TOO_SMALL;
    }

    ULONG struct_size = IDD_STRUCTURE_SIZE(IDDCX_MONITOR_MODE);
    for (uint32_t i = 0; i < written_count; ++i) {
        IDDCX_MONITOR_MODE* dst = &pInArgs->pDefaultMonitorModes[i];
        RtlZeroMemory(dst, sizeof(*dst));
        dst->Size = struct_size;
        dst->Origin = IDDCX_MONITOR_MODE_ORIGIN_DRIVER;
        fill_video_signal_info(&dst->MonitorVideoSignalInfo, &scratch[i]);
    }

    pOutArgs->DefaultMonitorModeBufferOutputCount = written_count;
    pOutArgs->PreferredMonitorModeIdx = final_preferred_idx;
    free_scratch_signals(scratch, is_heap);
    return STATUS_SUCCESS;
}

static NTSTATUS handle_query_target_modes_common(
    IDDCX_MONITOR MonitorObject,
    UINT input_count,
    PVOID out_modes_raw,
    UINT* out_count_ptr,
    bool is_v2
) {
    if (out_count_ptr == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *out_count_ptr = 0;

    if (g_callbacks.evt_query_target_modes == NULL) {
        return STATUS_NOT_SUPPORTED;
    }

    PZEROVDD_MONITOR_CTX mctx = ZeroVddGetMonitorContext(MonitorObject);
    PVOID dev = (mctx != NULL) ? mctx->Device : NULL;

    uint32_t required_count = 0;
    NTSTATUS status = (NTSTATUS)g_callbacks.evt_query_target_modes(
        (zerovdd_wdf_device_t)dev,
        (zerovdd_iddcx_monitor_t)MonitorObject,
        0u,
        NULL,
        &required_count
    );
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (required_count == 0u) {
        return STATUS_NO_DATA_DETECTED;
    }

    *out_count_ptr = required_count;

    if (out_modes_raw == NULL) {
        return (input_count == 0u) ? STATUS_SUCCESS : STATUS_INVALID_PARAMETER;
    }
    if (input_count < required_count) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    zerovdd_video_signal_t stack_buf[ZEROVDD_STACK_MODE_SCRATCH];
    bool is_heap = false;
    zerovdd_video_signal_t* scratch = alloc_scratch_signals(required_count, stack_buf, &is_heap);
    if (scratch == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    uint32_t written_count = 0;
    status = (NTSTATUS)g_callbacks.evt_query_target_modes(
        (zerovdd_wdf_device_t)dev,
        (zerovdd_iddcx_monitor_t)MonitorObject,
        required_count,
        scratch,
        &written_count
    );
    if (!NT_SUCCESS(status)) {
        free_scratch_signals(scratch, is_heap);
        return status;
    }
    if (written_count == 0u) {
        free_scratch_signals(scratch, is_heap);
        return STATUS_NO_DATA_DETECTED;
    }
    if (written_count > input_count) {
        *out_count_ptr = written_count;
        free_scratch_signals(scratch, is_heap);
        return STATUS_BUFFER_TOO_SMALL;
    }

    if (is_v2) {
        IDDCX_TARGET_MODE2* dst = (IDDCX_TARGET_MODE2*)out_modes_raw;
        ULONG struct_size = IDD_STRUCTURE_SIZE(IDDCX_TARGET_MODE2);
        for (uint32_t i = 0; i < written_count; ++i) {
            RtlZeroMemory(&dst[i], sizeof(IDDCX_TARGET_MODE2));
            dst[i].Size = struct_size;
            fill_video_signal_info(&dst[i].TargetVideoSignalInfo.targetVideoSignalInfo, &scratch[i]);
            dst[i].RequiredBandwidth = 0;
            dst[i].BitsPerComponent.Rgb = map_bpc(scratch[i].bits_per_component_rgb);
        }
    } else {
        IDDCX_TARGET_MODE* dst = (IDDCX_TARGET_MODE*)out_modes_raw;
        ULONG struct_size = IDD_STRUCTURE_SIZE(IDDCX_TARGET_MODE);
        for (uint32_t i = 0; i < written_count; ++i) {
            RtlZeroMemory(&dst[i], sizeof(IDDCX_TARGET_MODE));
            dst[i].Size = struct_size;
            fill_video_signal_info(&dst[i].TargetVideoSignalInfo.targetVideoSignalInfo, &scratch[i]);
            dst[i].RequiredBandwidth = 0;
        }
    }

    *out_count_ptr = written_count;
    free_scratch_signals(scratch, is_heap);
    return STATUS_SUCCESS;
}

static NTSTATUS NTAPI zerovdd_stub_monitor_query_target_modes(
    IDDCX_MONITOR MonitorObject,
    const IDARG_IN_QUERYTARGETMODES* pInArgs,
    IDARG_OUT_QUERYTARGETMODES* pOutArgs
) {
    if (pInArgs == NULL || pOutArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    return handle_query_target_modes_common(
        MonitorObject,
        pInArgs->TargetModeBufferInputCount,
        pInArgs->pTargetModes,
        &pOutArgs->TargetModeBufferOutputCount,
        false
    );
}

static NTSTATUS NTAPI zerovdd_stub_monitor_query_target_modes2(
    IDDCX_MONITOR MonitorObject,
    const IDARG_IN_QUERYTARGETMODES2* pInArgs,
    IDARG_OUT_QUERYTARGETMODES* pOutArgs
) {
    if (pInArgs == NULL || pOutArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    return handle_query_target_modes_common(
        MonitorObject,
        pInArgs->TargetModeBufferInputCount,
        pInArgs->pTargetModes,
        &pOutArgs->TargetModeBufferOutputCount,
        true
    );
}

static NTSTATUS NTAPI zerovdd_stub_monitor_set_default_hdr_metadata(
    IDDCX_MONITOR MonitorObject,
    const IDARG_IN_MONITOR_SET_DEFAULT_HDR_METADATA* pInArgs
) {
    if (pInArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (g_callbacks.evt_set_default_hdr_metadata == NULL) {
        return STATUS_SUCCESS;
    }

    PZEROVDD_MONITOR_CTX mctx = ZeroVddGetMonitorContext(MonitorObject);
    PVOID dev = (mctx != NULL) ? mctx->Device : NULL;

    zerovdd_hdr10_metadata_t meta = {0};
    const zerovdd_hdr10_metadata_t* meta_ptr = NULL;
    if (pInArgs->Type == IDDCX_HDRMETADATA_TYPE_HDR10 && pInArgs->Data.pHdr10 != NULL) {
        const IDDCX_HDR10_METADATA* src = pInArgs->Data.pHdr10;
        meta.red_primary[0] = src->RedPrimary[0];
        meta.red_primary[1] = src->RedPrimary[1];
        meta.green_primary[0] = src->GreenPrimary[0];
        meta.green_primary[1] = src->GreenPrimary[1];
        meta.blue_primary[0] = src->BluePrimary[0];
        meta.blue_primary[1] = src->BluePrimary[1];
        meta.white_point[0] = src->WhitePoint[0];
        meta.white_point[1] = src->WhitePoint[1];
        meta.max_mastering_luminance = src->MaxMasteringLuminance;
        meta.min_mastering_luminance = src->MinMasteringLuminance;
        meta.max_content_light_level = src->MaxContentLightLevel;
        meta.max_frame_average_light_level = src->MaxFrameAverageLightLevel;
        meta_ptr = &meta;
    }

    return (NTSTATUS)g_callbacks.evt_set_default_hdr_metadata(
        (zerovdd_wdf_device_t)dev,
        (zerovdd_iddcx_monitor_t)MonitorObject,
        (uint32_t)pInArgs->Type,
        meta_ptr
    );
}

static NTSTATUS NTAPI zerovdd_stub_monitor_set_gamma_ramp(
    IDDCX_MONITOR MonitorObject,
    const IDARG_IN_SET_GAMMARAMP* pInArgs
) {
    if (pInArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (g_callbacks.evt_set_gamma_ramp == NULL) {
        return STATUS_SUCCESS;
    }

    PZEROVDD_MONITOR_CTX mctx = ZeroVddGetMonitorContext(MonitorObject);
    PVOID dev = (mctx != NULL) ? mctx->Device : NULL;

    return (NTSTATUS)g_callbacks.evt_set_gamma_ramp(
        (zerovdd_wdf_device_t)dev,
        (zerovdd_iddcx_monitor_t)MonitorObject,
        (uint32_t)pInArgs->Type,
        (uint32_t)pInArgs->GammaRampSizeInBytes
    );
}

static NTSTATUS NTAPI zerovdd_stub_monitor_assign_swapchain(
    IDDCX_MONITOR MonitorObject,
    const IDARG_IN_SETSWAPCHAIN* pInArgs
) {
    if (pInArgs == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    if (g_callbacks.evt_assign_swapchain == NULL) {
        return STATUS_NOT_SUPPORTED;
    }

    PZEROVDD_MONITOR_CTX mctx = ZeroVddGetMonitorContext(MonitorObject);
    PVOID dev = (mctx != NULL) ? mctx->Device : NULL;

    return (NTSTATUS)g_callbacks.evt_assign_swapchain(
        (zerovdd_wdf_device_t)dev,
        (zerovdd_iddcx_monitor_t)MonitorObject,
        (zerovdd_iddcx_swapchain_t)pInArgs->hSwapChain,
        (zerovdd_handle_t)pInArgs->hNextSurfaceAvailable,
        (uint32_t)pInArgs->RenderAdapterLuid.LowPart,
        (int32_t)pInArgs->RenderAdapterLuid.HighPart
    );
}

static NTSTATUS NTAPI zerovdd_stub_monitor_unassign_swapchain(
    IDDCX_MONITOR MonitorObject
) {
    if (g_callbacks.evt_unassign_swapchain == NULL) {
        return STATUS_SUCCESS;
    }

    PZEROVDD_MONITOR_CTX mctx = ZeroVddGetMonitorContext(MonitorObject);
    PVOID dev = (mctx != NULL) ? mctx->Device : NULL;

    return (NTSTATUS)g_callbacks.evt_unassign_swapchain(
        (zerovdd_wdf_device_t)dev,
        (zerovdd_iddcx_monitor_t)MonitorObject
    );
}

extern "C" {

uint32_t zerovdd_iddcx_probe_v110(void) {
    uint32_t caps = 0;
    if (IDD_IS_FUNCTION_AVAILABLE(IddCxAdapterSetRenderAdapter)) {
        caps |= ZEROVDD_IDDCX_CAP_SET_RENDER_ADAPTER;
    }
    if (IDD_IS_FUNCTION_AVAILABLE(IddCxAdapterDisplayConfigUpdate2)) {
        caps |= ZEROVDD_IDDCX_CAP_DISPLAY_CONFIG_UPDATE2;
    }
    if (IDD_IS_FUNCTION_AVAILABLE(IddCxMonitorUpdateModes2)) {
        caps |= ZEROVDD_IDDCX_CAP_MONITOR_UPDATE_MODES2;
    }
    if (IDD_IS_FUNCTION_AVAILABLE(IddCxMonitorQueryHardwareCursor3)) {
        caps |= ZEROVDD_IDDCX_CAP_QUERY_HW_CURSOR3;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDDCX_ADAPTER_CAPS, Flags)) {
        caps |= ZEROVDD_IDDCX_CAP_ADAPTER_CAPS_FLAGS;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxParseMonitorDescription2)) {
        caps |= ZEROVDD_IDDCX_CAP_PARSE_DESC2;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxMonitorQueryTargetModes2)) {
        caps |= ZEROVDD_IDDCX_CAP_QUERY_MODES2;
    }
    if (IDD_IS_FUNCTION_AVAILABLE(IddCxSwapChainReleaseAndAcquireBuffer2)) {
        caps |= ZEROVDD_IDDCX_CAP_RELEASE_ACQUIRE2;
    }
    if (IDD_IS_FUNCTION_AVAILABLE(IddCxMonitorQueryHardwareCursor2)) {
        caps |= ZEROVDD_IDDCX_CAP_QUERY_HW_CURSOR2;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterQueryTargetInfo)) {
        caps |= ZEROVDD_IDDCX_CAP_QUERY_TARGET_INFO;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterCommitModes2)) {
        caps |= ZEROVDD_IDDCX_CAP_COMMIT_MODES2;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxMonitorSetDefaultHdrMetaData)) {
        caps |= ZEROVDD_IDDCX_CAP_SET_DEFAULT_HDR_META;
    }
    return caps;
}

uint8_t zerovdd_iddcx_supports_fp16(uint32_t caps) {
    const uint32_t required =
        ZEROVDD_IDDCX_CAP_ADAPTER_CAPS_FLAGS |
        ZEROVDD_IDDCX_CAP_PARSE_DESC2 |
        ZEROVDD_IDDCX_CAP_QUERY_MODES2 |
        ZEROVDD_IDDCX_CAP_RELEASE_ACQUIRE2 |
        ZEROVDD_IDDCX_CAP_QUERY_TARGET_INFO |
        ZEROVDD_IDDCX_CAP_COMMIT_MODES2 |
        ZEROVDD_IDDCX_CAP_SET_DEFAULT_HDR_META;
    return ((caps & required) == required) ? 1u : 0u;
}

zerovdd_ntstatus_t zerovdd_iddcx_get_version(uint32_t* out_version) {
    if (out_version == NULL) {
        return (zerovdd_ntstatus_t)STATUS_INVALID_PARAMETER;
    }
    IDARG_OUT_GETVERSION out_args = {0};
    NTSTATUS status = IddCxGetVersion(&out_args);
    if (NT_SUCCESS(status)) {
        *out_version = (uint32_t)out_args.IddCxVersion;
    }
    return (zerovdd_ntstatus_t)status;
}

zerovdd_ntstatus_t zerovdd_iddcx_device_init_config(
    zerovdd_wdf_device_init_t device_init,
    const zerovdd_iddcx_callbacks_t* callbacks
) {
    if (device_init == NULL || callbacks == NULL) {
        return (zerovdd_ntstatus_t)STATUS_INVALID_PARAMETER;
    }
    g_callbacks = *callbacks;

    IDD_CX_CLIENT_CONFIG config;
    IDD_CX_CLIENT_CONFIG_INIT(&config);
    config.EvtIddCxDeviceIoControl = (PFN_IDD_CX_DEVICE_IO_CONTROL)callbacks->evt_io_control;
    config.EvtIddCxParseMonitorDescription = zerovdd_stub_parse_monitor_description;
    config.EvtIddCxAdapterInitFinished = zerovdd_stub_adapter_init_finished;
    config.EvtIddCxAdapterCommitModes = zerovdd_stub_adapter_commit_modes;
    config.EvtIddCxMonitorGetDefaultDescriptionModes = zerovdd_stub_monitor_get_default_modes;
    config.EvtIddCxMonitorQueryTargetModes = zerovdd_stub_monitor_query_target_modes;
    config.EvtIddCxMonitorAssignSwapChain = zerovdd_stub_monitor_assign_swapchain;
    config.EvtIddCxMonitorUnassignSwapChain = zerovdd_stub_monitor_unassign_swapchain;
    config.EvtIddCxMonitorSetGammaRamp = zerovdd_stub_monitor_set_gamma_ramp;

    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxParseMonitorDescription2)) {
        config.EvtIddCxParseMonitorDescription2 = zerovdd_stub_parse_monitor_description2;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterQueryTargetInfo)) {
        config.EvtIddCxAdapterQueryTargetInfo = zerovdd_stub_adapter_query_target_info;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxAdapterCommitModes2)) {
        config.EvtIddCxAdapterCommitModes2 = zerovdd_stub_adapter_commit_modes2;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxMonitorSetDefaultHdrMetaData)) {
        config.EvtIddCxMonitorSetDefaultHdrMetaData = zerovdd_stub_monitor_set_default_hdr_metadata;
    }
    if (IDD_IS_FIELD_AVAILABLE(IDD_CX_CLIENT_CONFIG, EvtIddCxMonitorQueryTargetModes2)) {
        config.EvtIddCxMonitorQueryTargetModes2 = zerovdd_stub_monitor_query_target_modes2;
    }

    return (zerovdd_ntstatus_t)IddCxDeviceInitConfig((PWDFDEVICE_INIT)device_init, &config);
}

zerovdd_ntstatus_t zerovdd_iddcx_device_initialize(
    zerovdd_wdf_device_t device
) {
    return (zerovdd_ntstatus_t)IddCxDeviceInitialize((WDFDEVICE)device);
}

zerovdd_ntstatus_t zerovdd_iddcx_adapter_init_async(
    zerovdd_wdf_device_t device,
    uint32_t max_monitors,
    uint8_t can_process_fp16,
    zerovdd_iddcx_adapter_t* out_adapter
) {
    IDDCX_ADAPTER_CAPS caps;
    RtlZeroMemory(&caps, sizeof(caps));
    caps.Size = IDD_STRUCTURE_SIZE(IDDCX_ADAPTER_CAPS);
    if (can_process_fp16 != 0u && IDD_IS_FIELD_AVAILABLE(IDDCX_ADAPTER_CAPS, Flags)) {
        caps.Flags = IDDCX_ADAPTER_FLAGS_CAN_PROCESS_FP16;
    }
    caps.MaxDisplayPipelineRate = 0;
    IDDCX_ENDPOINT_VERSION endpoint_version;
    RtlZeroMemory(&endpoint_version, sizeof(endpoint_version));
    endpoint_version.Size = IDD_STRUCTURE_SIZE(IDDCX_ENDPOINT_VERSION);
    endpoint_version.MajorVer = 0;
    endpoint_version.MinorVer = 1;
    endpoint_version.Build = 0;
    endpoint_version.SKU = 0;

    caps.MaxMonitorsSupported = max_monitors;
    caps.EndPointDiagnostics.Size = IDD_STRUCTURE_SIZE(IDDCX_ENDPOINT_DIAGNOSTIC_INFO);
    caps.EndPointDiagnostics.TransmissionType = IDDCX_TRANSMISSION_TYPE_OTHER;
    caps.EndPointDiagnostics.pEndPointFriendlyName = L"ZeroVDD Virtual Display";
    caps.EndPointDiagnostics.pEndPointModelName = L"zerovdd-virtual";
    caps.EndPointDiagnostics.pEndPointManufacturerName = L"zerovdd";
    caps.EndPointDiagnostics.pHardwareVersion = &endpoint_version;
    caps.EndPointDiagnostics.pFirmwareVersion = &endpoint_version;
    caps.EndPointDiagnostics.GammaSupport = IDDCX_FEATURE_IMPLEMENTATION_SOFTWARE;

    WDF_OBJECT_ATTRIBUTES adapter_attrs;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&adapter_attrs, ZEROVDD_ADAPTER_CTX);
    // WHY: IddCx assigns WdfDevice as parent internally and rejects non-NULL ParentObject with STATUS_INVALID_PARAMETER.
    adapter_attrs.ParentObject = NULL;

    IDARG_IN_ADAPTER_INIT in_args;
    RtlZeroMemory(&in_args, sizeof(in_args));
    in_args.WdfDevice = (WDFDEVICE)device;
    in_args.pCaps = &caps;
    in_args.ObjectAttributes = &adapter_attrs;

    IDARG_OUT_ADAPTER_INIT out_args;
    RtlZeroMemory(&out_args, sizeof(out_args));

    NTSTATUS status = IddCxAdapterInitAsync(&in_args, &out_args);
    if (NT_SUCCESS(status)) {
        if (out_adapter != NULL) {
            *out_adapter = (zerovdd_iddcx_adapter_t)out_args.AdapterObject;
        }
        PZEROVDD_ADAPTER_CTX ctx = ZeroVddGetAdapterContext(out_args.AdapterObject);
        if (ctx != NULL) {
            ctx->ReadyCallback = g_callbacks.evt_adapter_ready;
            InterlockedExchangePointer(&ctx->Device, device);
            if (InterlockedCompareExchange(&ctx->InitFinished, 0, 0) != 0 &&
                ctx->ReadyCallback != NULL &&
                InterlockedCompareExchange(&ctx->ReadyDispatched, 1, 0) == 0) {
                NTSTATUS finished_status = InterlockedCompareExchange(&ctx->InitStatus, 0, 0);
                ctx->ReadyCallback(device, (zerovdd_ntstatus_t)finished_status);
            }
        }
    }
    return (zerovdd_ntstatus_t)status;
}

zerovdd_ntstatus_t zerovdd_iddcx_adapter_set_render_adapter(
    zerovdd_iddcx_adapter_t adapter,
    uint32_t luid_low,
    int32_t luid_high
) {
    if (adapter == NULL) {
        return (zerovdd_ntstatus_t)STATUS_INVALID_PARAMETER;
    }
    if (!IDD_IS_FUNCTION_AVAILABLE(IddCxAdapterSetRenderAdapter)) {
        return (zerovdd_ntstatus_t)STATUS_NOT_SUPPORTED;
    }
    IDARG_IN_ADAPTERSETRENDERADAPTER in_args;
    RtlZeroMemory(&in_args, sizeof(in_args));
    in_args.PreferredRenderAdapter.LowPart = luid_low;
    in_args.PreferredRenderAdapter.HighPart = luid_high;
    IddCxAdapterSetRenderAdapter((IDDCX_ADAPTER)adapter, &in_args);
    return (zerovdd_ntstatus_t)STATUS_SUCCESS;
}

zerovdd_ntstatus_t zerovdd_iddcx_monitor_create(
    zerovdd_wdf_device_t device,
    zerovdd_iddcx_adapter_t adapter,
    uint32_t connector_index,
    const uint8_t monitor_guid[16],
    const uint8_t* edid_data,
    uint32_t edid_size,
    zerovdd_iddcx_monitor_t* out_monitor
) {
    if (device == NULL || adapter == NULL || monitor_guid == NULL || out_monitor == NULL) {
        return (zerovdd_ntstatus_t)STATUS_INVALID_PARAMETER;
    }
    if (edid_size > 0u && edid_data == NULL) {
        return (zerovdd_ntstatus_t)STATUS_INVALID_PARAMETER;
    }
    *out_monitor = NULL;

    IDDCX_MONITOR_INFO info;
    RtlZeroMemory(&info, sizeof(info));
    info.Size = IDD_STRUCTURE_SIZE(IDDCX_MONITOR_INFO);
    info.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL;
    info.ConnectorIndex = connector_index;
    info.MonitorDescription.Size = IDD_STRUCTURE_SIZE(IDDCX_MONITOR_DESCRIPTION);
    info.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    info.MonitorDescription.DataSize = edid_size;
    info.MonitorDescription.pData = (PVOID)edid_data;

    info.MonitorContainerId.Data1 =
        ((uint32_t)monitor_guid[0] << 24) |
        ((uint32_t)monitor_guid[1] << 16) |
        ((uint32_t)monitor_guid[2] << 8) |
        (uint32_t)monitor_guid[3];
    info.MonitorContainerId.Data2 = (uint16_t)(((uint16_t)monitor_guid[4] << 8) | monitor_guid[5]);
    info.MonitorContainerId.Data3 = (uint16_t)(((uint16_t)monitor_guid[6] << 8) | monitor_guid[7]);
    RtlCopyMemory(info.MonitorContainerId.Data4, monitor_guid + 8, 8);

    WDF_OBJECT_ATTRIBUTES monitor_attrs;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&monitor_attrs, ZEROVDD_MONITOR_CTX);
    // WHY: IddCx assigns IDDCX_ADAPTER as parent internally and rejects non-NULL ParentObject with STATUS_INVALID_PARAMETER.
    monitor_attrs.ParentObject = NULL;

    IDARG_IN_MONITORCREATE in_args;
    RtlZeroMemory(&in_args, sizeof(in_args));
    in_args.ObjectAttributes = &monitor_attrs;
    in_args.pMonitorInfo = &info;

    IDARG_OUT_MONITORCREATE out_args;
    RtlZeroMemory(&out_args, sizeof(out_args));

    NTSTATUS status = IddCxMonitorCreate((IDDCX_ADAPTER)adapter, &in_args, &out_args);
    if (NT_SUCCESS(status)) {
        PZEROVDD_MONITOR_CTX mctx = ZeroVddGetMonitorContext(out_args.MonitorObject);
        if (mctx != NULL) {
            mctx->Device = device;
            mctx->ConnectorIndex = connector_index;
        }
        *out_monitor = (zerovdd_iddcx_monitor_t)out_args.MonitorObject;
    }
    return (zerovdd_ntstatus_t)status;
}

zerovdd_ntstatus_t zerovdd_iddcx_monitor_arrival(
    zerovdd_iddcx_monitor_t monitor,
    uint32_t* out_luid_low,
    int32_t* out_luid_high,
    uint32_t* out_target_id
) {
    if (monitor == NULL || out_luid_low == NULL || out_luid_high == NULL || out_target_id == NULL) {
        return (zerovdd_ntstatus_t)STATUS_INVALID_PARAMETER;
    }
    IDARG_OUT_MONITORARRIVAL out_args;
    RtlZeroMemory(&out_args, sizeof(out_args));
    NTSTATUS status = IddCxMonitorArrival((IDDCX_MONITOR)monitor, &out_args);
    if (NT_SUCCESS(status)) {
        *out_luid_low = (uint32_t)out_args.OsAdapterLuid.LowPart;
        *out_luid_high = (int32_t)out_args.OsAdapterLuid.HighPart;
        *out_target_id = (uint32_t)out_args.OsTargetId;
    }
    return (zerovdd_ntstatus_t)status;
}

zerovdd_ntstatus_t zerovdd_iddcx_monitor_departure(
    zerovdd_iddcx_monitor_t monitor
) {
    if (monitor == NULL) {
        return (zerovdd_ntstatus_t)STATUS_INVALID_PARAMETER;
    }
    return (zerovdd_ntstatus_t)IddCxMonitorDeparture((IDDCX_MONITOR)monitor);
}

zerovdd_ntstatus_t zerovdd_iddcx_monitor_update_modes(
    zerovdd_iddcx_monitor_t monitor,
    uint8_t use_v2,
    const zerovdd_video_signal_t* modes,
    uint32_t mode_count
) {
    if (monitor == NULL || modes == NULL || mode_count == 0u || mode_count > 1024u) {
        return (zerovdd_ntstatus_t)STATUS_INVALID_PARAMETER;
    }

    if (use_v2 != 0u && IDD_IS_FUNCTION_AVAILABLE(IddCxMonitorUpdateModes2)) {
        IDDCX_TARGET_MODE2 stack_modes[ZEROVDD_STACK_MODE_SCRATCH];
        IDDCX_TARGET_MODE2* target_modes = stack_modes;
        bool is_heap = false;
        if (mode_count > ZEROVDD_STACK_MODE_SCRATCH) {
            HANDLE heap = GetProcessHeap();
            if (heap == NULL) {
                return (zerovdd_ntstatus_t)STATUS_INSUFFICIENT_RESOURCES;
            }
            target_modes = (IDDCX_TARGET_MODE2*)HeapAlloc(
                heap,
                HEAP_ZERO_MEMORY,
                (SIZE_T)mode_count * sizeof(IDDCX_TARGET_MODE2)
            );
            if (target_modes == NULL) {
                return (zerovdd_ntstatus_t)STATUS_INSUFFICIENT_RESOURCES;
            }
            is_heap = true;
        } else {
            RtlZeroMemory(stack_modes, (SIZE_T)mode_count * sizeof(IDDCX_TARGET_MODE2));
        }

        ULONG struct_size = IDD_STRUCTURE_SIZE(IDDCX_TARGET_MODE2);
        for (uint32_t i = 0; i < mode_count; ++i) {
            target_modes[i].Size = struct_size;
            fill_video_signal_info(
                &target_modes[i].TargetVideoSignalInfo.targetVideoSignalInfo,
                &modes[i]
            );
            target_modes[i].RequiredBandwidth = 0;
            target_modes[i].BitsPerComponent.Rgb = map_bpc(modes[i].bits_per_component_rgb);
        }

        IDARG_IN_UPDATEMODES2 in_args;
        RtlZeroMemory(&in_args, sizeof(in_args));
        in_args.Reason = IDDCX_UPDATE_REASON_OTHER;
        in_args.TargetModeCount = mode_count;
        in_args.pTargetModes = target_modes;

        NTSTATUS status = IddCxMonitorUpdateModes2((IDDCX_MONITOR)monitor, &in_args);
        if (is_heap) {
            HANDLE heap = GetProcessHeap();
            if (heap != NULL) {
                HeapFree(heap, 0, target_modes);
            }
        }
        return (zerovdd_ntstatus_t)status;
    }

    IDDCX_TARGET_MODE stack_modes[ZEROVDD_STACK_MODE_SCRATCH];
    IDDCX_TARGET_MODE* target_modes = stack_modes;
    bool is_heap = false;
    if (mode_count > ZEROVDD_STACK_MODE_SCRATCH) {
        HANDLE heap = GetProcessHeap();
        if (heap == NULL) {
            return (zerovdd_ntstatus_t)STATUS_INSUFFICIENT_RESOURCES;
        }
        target_modes = (IDDCX_TARGET_MODE*)HeapAlloc(
            heap,
            HEAP_ZERO_MEMORY,
            (SIZE_T)mode_count * sizeof(IDDCX_TARGET_MODE)
        );
        if (target_modes == NULL) {
            return (zerovdd_ntstatus_t)STATUS_INSUFFICIENT_RESOURCES;
        }
        is_heap = true;
    } else {
        RtlZeroMemory(stack_modes, (SIZE_T)mode_count * sizeof(IDDCX_TARGET_MODE));
    }

    ULONG struct_size = IDD_STRUCTURE_SIZE(IDDCX_TARGET_MODE);
    for (uint32_t i = 0; i < mode_count; ++i) {
        target_modes[i].Size = struct_size;
        fill_video_signal_info(
            &target_modes[i].TargetVideoSignalInfo.targetVideoSignalInfo,
            &modes[i]
        );
        target_modes[i].RequiredBandwidth = 0;
    }

    IDARG_IN_UPDATEMODES in_args;
    RtlZeroMemory(&in_args, sizeof(in_args));
    in_args.Reason = IDDCX_UPDATE_REASON_OTHER;
    in_args.TargetModeCount = mode_count;
    in_args.pTargetModes = target_modes;

    NTSTATUS status = IddCxMonitorUpdateModes((IDDCX_MONITOR)monitor, &in_args);
    if (is_heap) {
        HANDLE heap = GetProcessHeap();
        if (heap != NULL) {
            HeapFree(heap, 0, target_modes);
        }
    }
    return (zerovdd_ntstatus_t)status;
}

zerovdd_ntstatus_t zerovdd_iddcx_monitor_setup_hw_cursor(
    zerovdd_iddcx_monitor_t monitor,
    zerovdd_handle_t cursor_event,
    uint32_t max_x,
    uint32_t max_y
) {
    if (monitor == NULL || cursor_event == NULL) {
        return (zerovdd_ntstatus_t)STATUS_INVALID_PARAMETER;
    }
    IDARG_IN_SETUP_HWCURSOR in_args;
    RtlZeroMemory(&in_args, sizeof(in_args));
    in_args.CursorInfo.Size = IDD_STRUCTURE_SIZE(IDDCX_CURSOR_CAPS);
    in_args.CursorInfo.ColorXorCursorSupport = IDDCX_XOR_CURSOR_SUPPORT_FULL;
    in_args.CursorInfo.AlphaCursorSupport = TRUE;
    in_args.CursorInfo.MaxX = max_x;
    in_args.CursorInfo.MaxY = max_y;
    in_args.hNewCursorDataAvailable = (HANDLE)cursor_event;
    return (zerovdd_ntstatus_t)IddCxMonitorSetupHardwareCursor((IDDCX_MONITOR)monitor, &in_args);
}

zerovdd_ntstatus_t zerovdd_iddcx_monitor_query_hw_cursor(
    zerovdd_iddcx_monitor_t monitor,
    uint32_t last_shape_id,
    uint8_t* shape_buffer,
    uint32_t shape_buffer_size,
    zerovdd_cursor_info_t* out_info
) {
    if (monitor == NULL || out_info == NULL) {
        return (zerovdd_ntstatus_t)STATUS_INVALID_PARAMETER;
    }
    if (shape_buffer_size > 0u && shape_buffer == NULL) {
        return (zerovdd_ntstatus_t)STATUS_INVALID_PARAMETER;
    }
    RtlZeroMemory(out_info, sizeof(*out_info));

    IDARG_IN_QUERY_HWCURSOR in_args;
    RtlZeroMemory(&in_args, sizeof(in_args));
    in_args.LastShapeId = last_shape_id;
    in_args.ShapeBufferSizeInBytes = shape_buffer_size;
    in_args.pShapeBuffer = (PBYTE)shape_buffer;

    if (IDD_IS_FUNCTION_AVAILABLE(IddCxMonitorQueryHardwareCursor3)) {
        IDARG_OUT_QUERY_HWCURSOR3 out3;
        RtlZeroMemory(&out3, sizeof(out3));
        HRESULT hr = IddCxMonitorQueryHardwareCursor3((IDDCX_MONITOR)monitor, &in_args, &out3);
        if (SUCCEEDED(hr)) {
            out_info->is_visible = out3.IsCursorVisible ? 1u : 0u;
            out_info->x = out3.X;
            out_info->y = out3.Y;
            out_info->is_shape_updated = out3.IsCursorShapeUpdated ? 1u : 0u;
            out_info->shape_id = out3.CursorShapeInfo.ShapeId;
            out_info->cursor_type = (uint32_t)out3.CursorShapeInfo.CursorType;
            out_info->width = out3.CursorShapeInfo.Width;
            out_info->height = out3.CursorShapeInfo.Height;
            out_info->pitch = out3.CursorShapeInfo.Pitch;
            out_info->x_hot = out3.CursorShapeInfo.XHot;
            out_info->y_hot = out3.CursorShapeInfo.YHot;
            out_info->position_valid = out3.PositionValid ? 1u : 0u;
            out_info->position_id = out3.PositionId;
            out_info->sdr_white_level = out3.SdrWhiteLevel;
        }
        return (zerovdd_ntstatus_t)hr;
    }

    if (IDD_IS_FUNCTION_AVAILABLE(IddCxMonitorQueryHardwareCursor2)) {
        IDARG_OUT_QUERY_HWCURSOR2 out2;
        RtlZeroMemory(&out2, sizeof(out2));
        NTSTATUS st = IddCxMonitorQueryHardwareCursor2((IDDCX_MONITOR)monitor, &in_args, &out2);
        if (NT_SUCCESS(st)) {
            out_info->is_visible = out2.IsCursorVisible ? 1u : 0u;
            out_info->x = out2.X;
            out_info->y = out2.Y;
            out_info->is_shape_updated = out2.IsCursorShapeUpdated ? 1u : 0u;
            out_info->shape_id = out2.CursorShapeInfo.ShapeId;
            out_info->cursor_type = (uint32_t)out2.CursorShapeInfo.CursorType;
            out_info->width = out2.CursorShapeInfo.Width;
            out_info->height = out2.CursorShapeInfo.Height;
            out_info->pitch = out2.CursorShapeInfo.Pitch;
            out_info->x_hot = out2.CursorShapeInfo.XHot;
            out_info->y_hot = out2.CursorShapeInfo.YHot;
            out_info->position_valid = out2.PositionValid ? 1u : 0u;
            out_info->position_id = out2.PositionId;
        }
        return (zerovdd_ntstatus_t)st;
    }

    IDARG_OUT_QUERY_HWCURSOR out1;
    RtlZeroMemory(&out1, sizeof(out1));
    NTSTATUS st = IddCxMonitorQueryHardwareCursor((IDDCX_MONITOR)monitor, &in_args, &out1);
    if (NT_SUCCESS(st)) {
        out_info->is_visible = out1.IsCursorVisible ? 1u : 0u;
        out_info->x = out1.X;
        out_info->y = out1.Y;
        out_info->is_shape_updated = out1.IsCursorShapeUpdated ? 1u : 0u;
        out_info->shape_id = out1.CursorShapeInfo.ShapeId;
        out_info->cursor_type = (uint32_t)out1.CursorShapeInfo.CursorType;
        out_info->width = out1.CursorShapeInfo.Width;
        out_info->height = out1.CursorShapeInfo.Height;
        out_info->pitch = out1.CursorShapeInfo.Pitch;
        out_info->x_hot = out1.CursorShapeInfo.XHot;
        out_info->y_hot = out1.CursorShapeInfo.YHot;
        out_info->position_valid = out1.IsCursorVisible ? 1u : 0u;
    }
    return (zerovdd_ntstatus_t)st;
}

zerovdd_hresult_t zerovdd_iddcx_swapchain_set_device(
    zerovdd_iddcx_swapchain_t swapchain,
    void* dxgi_device
) {
    if (swapchain == NULL || dxgi_device == NULL) {
        return (zerovdd_hresult_t)E_INVALIDARG;
    }
    IDARG_IN_SWAPCHAINSETDEVICE in_args;
    RtlZeroMemory(&in_args, sizeof(in_args));
    in_args.pDevice = (IDXGIDevice*)dxgi_device;
    return (zerovdd_hresult_t)IddCxSwapChainSetDevice((IDDCX_SWAPCHAIN)swapchain, &in_args);
}

zerovdd_hresult_t zerovdd_iddcx_swapchain_release_and_acquire_buffer(
    zerovdd_iddcx_swapchain_t swapchain,
    uint8_t use_v2,
    zerovdd_frame_info_t* out_frame
) {
    if (swapchain == NULL || out_frame == NULL) {
        return (zerovdd_hresult_t)E_INVALIDARG;
    }
    RtlZeroMemory(out_frame, sizeof(*out_frame));

    if (use_v2 != 0u && IDD_IS_FUNCTION_AVAILABLE(IddCxSwapChainReleaseAndAcquireBuffer2)) {
        IDARG_IN_RELEASEANDACQUIREBUFFER2 in_args;
        RtlZeroMemory(&in_args, sizeof(in_args));
        in_args.Size = IDD_STRUCTURE_SIZE(IDARG_IN_RELEASEANDACQUIREBUFFER2);
        in_args.AcquireSystemMemoryBuffer = FALSE;

        IDARG_OUT_RELEASEANDACQUIREBUFFER2 out_args;
        RtlZeroMemory(&out_args, sizeof(out_args));

        HRESULT hr = IddCxSwapChainReleaseAndAcquireBuffer2(
            (IDDCX_SWAPCHAIN)swapchain,
            &in_args,
            &out_args
        );
        if (SUCCEEDED(hr)) {
            out_frame->presentation_frame_number = out_args.MetaData.PresentationFrameNumber;
            out_frame->dirty_rect_count = out_args.MetaData.DirtyRectCount;
            out_frame->move_region_count = 0u;
            out_frame->surface_color_space = (uint32_t)out_args.MetaData.SurfaceColorSpace;
            out_frame->sdr_white_level = out_args.MetaData.SdrWhiteLevel;
            out_frame->present_display_qpc_time = out_args.MetaData.PresentDisplayQPCTime;
            if (out_args.MetaData.pSurface != NULL) {
                out_args.MetaData.pSurface->Release();
                out_args.MetaData.pSurface = NULL;
            }
        }
        return (zerovdd_hresult_t)hr;
    }

    IDARG_OUT_RELEASEANDACQUIREBUFFER out1;
    RtlZeroMemory(&out1, sizeof(out1));
    HRESULT hr = IddCxSwapChainReleaseAndAcquireBuffer((IDDCX_SWAPCHAIN)swapchain, &out1);
    if (SUCCEEDED(hr)) {
        out_frame->presentation_frame_number = out1.MetaData.PresentationFrameNumber;
        out_frame->dirty_rect_count = out1.MetaData.DirtyRectCount;
        out_frame->move_region_count = out1.MetaData.MoveRegionCount;
        out_frame->present_display_qpc_time = out1.MetaData.PresentDisplayQPCTime;
        if (out1.MetaData.pSurface != NULL) {
            out1.MetaData.pSurface->Release();
            out1.MetaData.pSurface = NULL;
        }
    }
    return (zerovdd_hresult_t)hr;
}

zerovdd_hresult_t zerovdd_iddcx_swapchain_finished_processing_frame(
    zerovdd_iddcx_swapchain_t swapchain
) {
    if (swapchain == NULL) {
        return (zerovdd_hresult_t)E_INVALIDARG;
    }
    return (zerovdd_hresult_t)IddCxSwapChainFinishedProcessingFrame((IDDCX_SWAPCHAIN)swapchain);
}

zerovdd_hresult_t zerovdd_d3d_create_device_for_luid(
    uint32_t luid_low,
    int32_t luid_high,
    void** out_d3d11_device,
    void** out_dxgi_device,
    void** out_dxgi_factory
) {
    if (out_d3d11_device == NULL || out_dxgi_device == NULL || out_dxgi_factory == NULL) {
        return (zerovdd_hresult_t)E_INVALIDARG;
    }
    *out_d3d11_device = NULL;
    *out_dxgi_device = NULL;
    *out_dxgi_factory = NULL;

    IDXGIFactory1* factory = NULL;
    HRESULT hr = CreateDXGIFactory1(__uuidof(IDXGIFactory1), (void**)&factory);
    if (FAILED(hr) || factory == NULL) {
        return (zerovdd_hresult_t)hr;
    }

    const bool match_specific_luid = (luid_low != 0u) || (luid_high != 0);
    const D3D_FEATURE_LEVEL feature_levels[] = {
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0
    };

    for (UINT idx = 0;; ++idx) {
        IDXGIAdapter1* adapter = NULL;
        hr = factory->EnumAdapters1(idx, &adapter);
        if (hr == DXGI_ERROR_NOT_FOUND) {
            break;
        }
        if (FAILED(hr) || adapter == NULL) {
            factory->Release();
            return (zerovdd_hresult_t)hr;
        }

        DXGI_ADAPTER_DESC1 desc;
        RtlZeroMemory(&desc, sizeof(desc));
        hr = adapter->GetDesc1(&desc);
        if (FAILED(hr)) {
            adapter->Release();
            continue;
        }

        if ((desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) != 0u) {
            adapter->Release();
            continue;
        }

        if (match_specific_luid &&
            (desc.AdapterLuid.LowPart != luid_low || desc.AdapterLuid.HighPart != luid_high)) {
            adapter->Release();
            continue;
        }

        ID3D11Device* d3d_device = NULL;
        ID3D11DeviceContext* d3d_context = NULL;
        hr = D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            NULL,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            feature_levels,
            (UINT)ARRAYSIZE(feature_levels),
            D3D11_SDK_VERSION,
            &d3d_device,
            NULL,
            &d3d_context
        );
        adapter->Release();
        if (d3d_context != NULL) {
            d3d_context->Release();
            d3d_context = NULL;
        }
        if (FAILED(hr) || d3d_device == NULL) {
            if (match_specific_luid) {
                factory->Release();
                return (zerovdd_hresult_t)hr;
            }
            continue;
        }

        IDXGIDevice* dxgi_device = NULL;
        hr = d3d_device->QueryInterface(__uuidof(IDXGIDevice), (void**)&dxgi_device);
        if (FAILED(hr) || dxgi_device == NULL) {
            d3d_device->Release();
            factory->Release();
            return (zerovdd_hresult_t)hr;
        }

        *out_d3d11_device = (void*)d3d_device;
        *out_dxgi_device = (void*)dxgi_device;
        *out_dxgi_factory = (void*)factory;
        return (zerovdd_hresult_t)S_OK;
    }

    factory->Release();
    return (zerovdd_hresult_t)DXGI_ERROR_NOT_FOUND;
}

uint8_t zerovdd_d3d_is_device_valid(
    void* d3d11_device,
    void* dxgi_factory
) {
    if (d3d11_device == NULL || dxgi_factory == NULL) {
        return 0u;
    }
    IDXGIFactory1* factory = (IDXGIFactory1*)dxgi_factory;
    ID3D11Device* device = (ID3D11Device*)d3d11_device;
    if (!factory->IsCurrent()) {
        return 0u;
    }
    if (device->GetDeviceRemovedReason() != S_OK) {
        return 0u;
    }
    return 1u;
}

void zerovdd_d3d_flush_device(void* d3d11_device) {
    if (d3d11_device == NULL) {
        return;
    }
    ID3D11Device* device = (ID3D11Device*)d3d11_device;
    ID3D11DeviceContext* ctx = NULL;
    device->GetImmediateContext(&ctx);
    if (ctx != NULL) {
        ctx->ClearState();
        ctx->Flush();
        ctx->Release();
    }
}

void zerovdd_com_add_ref(void* com_obj) {
    if (com_obj != NULL) {
        ((IUnknown*)com_obj)->AddRef();
    }
}

void zerovdd_com_release(void* com_obj) {
    if (com_obj != NULL) {
        ((IUnknown*)com_obj)->Release();
    }
}

} // extern "C"
