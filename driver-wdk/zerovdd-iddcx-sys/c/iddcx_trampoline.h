#ifndef ZEROVDD_IDDCX_TRAMPOLINE_H
#define ZEROVDD_IDDCX_TRAMPOLINE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Capability probe bits returned by zerovdd_iddcx_probe_v110.
#define ZEROVDD_IDDCX_CAP_SET_RENDER_ADAPTER (1u << 0)
#define ZEROVDD_IDDCX_CAP_DISPLAY_CONFIG_UPDATE2 (1u << 1)
#define ZEROVDD_IDDCX_CAP_MONITOR_UPDATE_MODES2 (1u << 2)
#define ZEROVDD_IDDCX_CAP_QUERY_HW_CURSOR3 (1u << 3)
#define ZEROVDD_IDDCX_CAP_ADAPTER_CAPS_FLAGS (1u << 4)
#define ZEROVDD_IDDCX_CAP_PARSE_DESC2 (1u << 5)
#define ZEROVDD_IDDCX_CAP_QUERY_MODES2 (1u << 6)
#define ZEROVDD_IDDCX_CAP_RELEASE_ACQUIRE2 (1u << 7)
#define ZEROVDD_IDDCX_CAP_QUERY_HW_CURSOR2 (1u << 8)
#define ZEROVDD_IDDCX_CAP_QUERY_TARGET_INFO (1u << 9)
#define ZEROVDD_IDDCX_CAP_COMMIT_MODES2 (1u << 10)
#define ZEROVDD_IDDCX_CAP_SET_DEFAULT_HDR_META (1u << 11)

// Wire bits-per-component bitmask values matching IDDCX_BITS_PER_COMPONENT.
#define ZEROVDD_BPC_NONE 0u
#define ZEROVDD_BPC_8 0x02u
#define ZEROVDD_BPC_10 0x04u
#define ZEROVDD_BPC_12 0x08u

// Target capability flags matching IDDCX_TARGET_CAPS.
#define ZEROVDD_TARGET_CAPS_NONE 0u
#define ZEROVDD_TARGET_CAPS_WIDE_COLOR_SPACE 0x1u
#define ZEROVDD_TARGET_CAPS_HIGH_COLOR_SPACE 0x2u

// Path flags matching IDDCX_PATH_FLAGS.
#define ZEROVDD_PATH_FLAG_CHANGED 0x1u
#define ZEROVDD_PATH_FLAG_ACTIVE 0x2u

typedef void* zerovdd_wdf_device_init_t;
typedef void* zerovdd_wdf_device_t;
typedef void* zerovdd_wdf_request_t;
typedef void* zerovdd_iddcx_adapter_t;
typedef void* zerovdd_iddcx_monitor_t;
typedef void* zerovdd_iddcx_swapchain_t;
typedef void* zerovdd_handle_t;
typedef int32_t zerovdd_ntstatus_t;
typedef int32_t zerovdd_hresult_t;

typedef struct _zerovdd_video_signal_t {
    uint64_t pixel_rate;
    uint32_t active_width;
    uint32_t active_height;
    uint32_t total_width;
    uint32_t total_height;
    uint32_t hsync_freq_num;
    uint32_t hsync_freq_den;
    uint32_t vsync_freq_num;
    uint32_t vsync_freq_den;
    uint32_t vsync_freq_divider;
    uint32_t bits_per_component_rgb;
} zerovdd_video_signal_t;

typedef struct _zerovdd_target_info_t {
    uint32_t target_caps;
    uint32_t dithering_rgb;
} zerovdd_target_info_t;

typedef struct _zerovdd_commit_path_t {
    zerovdd_iddcx_monitor_t monitor;
    uint32_t flags;
    uint32_t active_width;
    uint32_t active_height;
    uint32_t vsync_num;
    uint32_t vsync_den;
    uint32_t color_space;
    uint32_t bits_per_component_rgb;
} zerovdd_commit_path_t;

typedef struct _zerovdd_hdr10_metadata_t {
    uint16_t red_primary[2];
    uint16_t green_primary[2];
    uint16_t blue_primary[2];
    uint16_t white_point[2];
    uint16_t max_mastering_luminance;
    uint16_t min_mastering_luminance;
    uint16_t max_content_light_level;
    uint16_t max_frame_average_light_level;
} zerovdd_hdr10_metadata_t;

typedef struct _zerovdd_cursor_info_t {
    uint32_t is_visible;
    int32_t x;
    int32_t y;
    uint32_t is_shape_updated;
    uint32_t shape_id;
    uint32_t cursor_type;
    uint32_t width;
    uint32_t height;
    uint32_t pitch;
    uint32_t x_hot;
    uint32_t y_hot;
    uint32_t position_valid;
    uint32_t position_id;
    uint32_t sdr_white_level;
} zerovdd_cursor_info_t;

typedef struct _zerovdd_frame_info_t {
    uint32_t presentation_frame_number;
    uint32_t dirty_rect_count;
    uint32_t move_region_count;
    uint32_t surface_color_space;
    uint32_t sdr_white_level;
    uint64_t present_display_qpc_time;
} zerovdd_frame_info_t;

typedef void(__stdcall* zerovdd_evt_device_io_control_t)(
    zerovdd_wdf_device_t device,
    zerovdd_wdf_request_t request,
    size_t output_buffer_length,
    size_t input_buffer_length,
    uint32_t io_control_code
);

typedef void(__stdcall* zerovdd_evt_adapter_ready_t)(
    zerovdd_wdf_device_t device,
    zerovdd_ntstatus_t adapter_init_status
);

typedef zerovdd_ntstatus_t(__stdcall* zerovdd_evt_query_target_info_t)(
    zerovdd_wdf_device_t device,
    uint32_t connector_index,
    zerovdd_target_info_t* out_info
);

typedef zerovdd_ntstatus_t(__stdcall* zerovdd_evt_parse_monitor_description_t)(
    const uint8_t* edid_data,
    uint32_t edid_size,
    uint32_t max_modes,
    zerovdd_video_signal_t* out_modes,
    uint32_t* out_count,
    uint32_t* out_preferred_idx
);

typedef zerovdd_ntstatus_t(__stdcall* zerovdd_evt_get_default_description_modes_t)(
    zerovdd_wdf_device_t device,
    zerovdd_iddcx_monitor_t monitor,
    uint32_t max_modes,
    zerovdd_video_signal_t* out_modes,
    uint32_t* out_count,
    uint32_t* out_preferred_idx
);

typedef zerovdd_ntstatus_t(__stdcall* zerovdd_evt_query_target_modes_t)(
    zerovdd_wdf_device_t device,
    zerovdd_iddcx_monitor_t monitor,
    uint32_t max_modes,
    zerovdd_video_signal_t* out_modes,
    uint32_t* out_count
);

typedef zerovdd_ntstatus_t(__stdcall* zerovdd_evt_commit_modes_t)(
    zerovdd_wdf_device_t device,
    uint32_t path_count,
    const zerovdd_commit_path_t* paths
);

typedef zerovdd_ntstatus_t(__stdcall* zerovdd_evt_set_default_hdr_metadata_t)(
    zerovdd_wdf_device_t device,
    zerovdd_iddcx_monitor_t monitor,
    uint32_t metadata_type,
    const zerovdd_hdr10_metadata_t* hdr10
);

typedef zerovdd_ntstatus_t(__stdcall* zerovdd_evt_set_gamma_ramp_t)(
    zerovdd_wdf_device_t device,
    zerovdd_iddcx_monitor_t monitor,
    uint32_t ramp_type,
    uint32_t ramp_size_bytes
);

typedef zerovdd_ntstatus_t(__stdcall* zerovdd_evt_assign_swapchain_t)(
    zerovdd_wdf_device_t device,
    zerovdd_iddcx_monitor_t monitor,
    zerovdd_iddcx_swapchain_t swapchain,
    zerovdd_handle_t next_surface_available_event,
    uint32_t render_luid_low,
    int32_t render_luid_high
);

typedef zerovdd_ntstatus_t(__stdcall* zerovdd_evt_unassign_swapchain_t)(
    zerovdd_wdf_device_t device,
    zerovdd_iddcx_monitor_t monitor
);

typedef struct _zerovdd_iddcx_callbacks_t {
    zerovdd_evt_device_io_control_t evt_io_control;
    zerovdd_evt_adapter_ready_t evt_adapter_ready;
    zerovdd_evt_query_target_info_t evt_query_target_info;
    zerovdd_evt_parse_monitor_description_t evt_parse_monitor_description;
    zerovdd_evt_get_default_description_modes_t evt_get_default_description_modes;
    zerovdd_evt_query_target_modes_t evt_query_target_modes;
    zerovdd_evt_commit_modes_t evt_commit_modes;
    zerovdd_evt_set_default_hdr_metadata_t evt_set_default_hdr_metadata;
    zerovdd_evt_set_gamma_ramp_t evt_set_gamma_ramp;
    zerovdd_evt_assign_swapchain_t evt_assign_swapchain;
    zerovdd_evt_unassign_swapchain_t evt_unassign_swapchain;
} zerovdd_iddcx_callbacks_t;

uint32_t zerovdd_iddcx_probe_v110(void);

uint8_t zerovdd_iddcx_supports_fp16(uint32_t caps);

zerovdd_ntstatus_t zerovdd_iddcx_get_version(uint32_t* out_version);

zerovdd_ntstatus_t zerovdd_iddcx_device_init_config(
    zerovdd_wdf_device_init_t device_init,
    const zerovdd_iddcx_callbacks_t* callbacks
);

zerovdd_ntstatus_t zerovdd_iddcx_device_initialize(
    zerovdd_wdf_device_t device
);

zerovdd_ntstatus_t zerovdd_iddcx_adapter_init_async(
    zerovdd_wdf_device_t device,
    uint32_t max_monitors,
    uint8_t can_process_fp16,
    zerovdd_iddcx_adapter_t* out_adapter
);

zerovdd_ntstatus_t zerovdd_iddcx_adapter_set_render_adapter(
    zerovdd_iddcx_adapter_t adapter,
    uint32_t luid_low,
    int32_t luid_high
);

zerovdd_ntstatus_t zerovdd_iddcx_monitor_create(
    zerovdd_wdf_device_t device,
    zerovdd_iddcx_adapter_t adapter,
    uint32_t connector_index,
    const uint8_t monitor_guid[16],
    const uint8_t* edid_data,
    uint32_t edid_size,
    zerovdd_iddcx_monitor_t* out_monitor
);

zerovdd_ntstatus_t zerovdd_iddcx_monitor_arrival(
    zerovdd_iddcx_monitor_t monitor,
    uint32_t* out_luid_low,
    int32_t* out_luid_high,
    uint32_t* out_target_id
);

zerovdd_ntstatus_t zerovdd_iddcx_monitor_departure(
    zerovdd_iddcx_monitor_t monitor
);

zerovdd_ntstatus_t zerovdd_iddcx_monitor_update_modes(
    zerovdd_iddcx_monitor_t monitor,
    uint8_t use_v2,
    const zerovdd_video_signal_t* modes,
    uint32_t mode_count
);

zerovdd_ntstatus_t zerovdd_iddcx_monitor_setup_hw_cursor(
    zerovdd_iddcx_monitor_t monitor,
    zerovdd_handle_t cursor_event,
    uint32_t max_x,
    uint32_t max_y
);

zerovdd_ntstatus_t zerovdd_iddcx_monitor_query_hw_cursor(
    zerovdd_iddcx_monitor_t monitor,
    uint32_t last_shape_id,
    uint8_t* shape_buffer,
    uint32_t shape_buffer_size,
    zerovdd_cursor_info_t* out_info
);

zerovdd_hresult_t zerovdd_iddcx_swapchain_set_device(
    zerovdd_iddcx_swapchain_t swapchain,
    void* dxgi_device
);

zerovdd_hresult_t zerovdd_iddcx_swapchain_release_and_acquire_buffer(
    zerovdd_iddcx_swapchain_t swapchain,
    uint8_t use_v2,
    zerovdd_frame_info_t* out_frame
);

zerovdd_hresult_t zerovdd_iddcx_swapchain_finished_processing_frame(
    zerovdd_iddcx_swapchain_t swapchain
);

zerovdd_hresult_t zerovdd_d3d_create_device_for_luid(
    uint32_t luid_low,
    int32_t luid_high,
    void** out_d3d11_device,
    void** out_dxgi_device,
    void** out_dxgi_factory
);

uint8_t zerovdd_d3d_is_device_valid(
    void* d3d11_device,
    void* dxgi_factory
);

void zerovdd_d3d_flush_device(
    void* d3d11_device
);

void zerovdd_com_add_ref(void* com_obj);

void zerovdd_com_release(void* com_obj);

#ifdef __cplusplus
}
#endif

#endif
