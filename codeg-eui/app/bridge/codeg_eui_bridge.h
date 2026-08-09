#pragma once

#include <stddef.h>
#include <stdint.h>

#define CODEG_EUI_API_VERSION 1u
#define CODEG_EUI_OK 0
#define CODEG_EUI_ERR_INVALID_STATE 1
#define CODEG_EUI_ERR_NULL_POINTER 2
#define CODEG_EUI_ERR_NOT_READY 9

typedef struct CodegEuiFrame {
    uint32_t api_version;
    uint32_t lifecycle_state;
    uint64_t generation;
    uint8_t shutdown_ready;
    uint8_t reserved[7];
} CodegEuiFrame;

#if defined(__cplusplus)
extern "C" {
#endif

uint32_t codeg_eui_api_version(void);
int codeg_eui_init(const uint8_t* data_dir_utf8, size_t data_dir_len);
int codeg_eui_poll(CodegEuiFrame* out);
int codeg_eui_begin_shutdown(void);
int codeg_eui_shutdown(void);

#if defined(__cplusplus)
}

static_assert(sizeof(CodegEuiFrame) == 24, "CodegEuiFrame ABI drift");
#endif
