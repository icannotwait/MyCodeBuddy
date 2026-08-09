#pragma once

#include <stddef.h>
#include <stdint.h>

#define CODEG_EUI_API_VERSION 1u
#define CODEG_EUI_OK 0
#define CODEG_EUI_ERR_INVALID_STATE 1
#define CODEG_EUI_ERR_NULL_POINTER 2
#define CODEG_EUI_ERR_INVALID_UTF8 3
#define CODEG_EUI_ERR_TOO_LARGE 4
#define CODEG_EUI_ERR_QUEUE_FULL 5
#define CODEG_EUI_ERR_WRONG_THREAD 6
#define CODEG_EUI_ERR_PANIC 7
#define CODEG_EUI_ERR_INTERNAL 8
#define CODEG_EUI_ERR_NOT_READY 9

#define CODEG_EUI_MAX_PATH_BYTES 32768u
#define CODEG_EUI_MAX_MESSAGE_BYTES 1048576u
#define CODEG_EUI_MAX_SETTINGS_JSON_BYTES 2097152u
#define CODEG_EUI_COMMAND_QUEUE_CAPACITY 256u
#define CODEG_EUI_COMPLETION_CAPACITY 256u

typedef enum CodegEuiLifecycleState {
    CODEG_EUI_LIFECYCLE_UNINITIALIZED = 0,
    CODEG_EUI_LIFECYCLE_STARTING = 1,
    CODEG_EUI_LIFECYCLE_RUNNING = 2,
    CODEG_EUI_LIFECYCLE_STOPPING = 3,
    CODEG_EUI_LIFECYCLE_STOPPED = 4,
} CodegEuiLifecycleState;

typedef enum CodegEuiOperation {
    CODEG_EUI_OP_SET_WORKSPACE = 1,
    CODEG_EUI_OP_CREATE_SESSION = 2,
    CODEG_EUI_OP_SELECT_SESSION = 3,
    CODEG_EUI_OP_SEND_USER_MESSAGE = 4,
    CODEG_EUI_OP_CANCEL_ACTIVE_TURN = 5,
    CODEG_EUI_OP_GET_AGENT_SETTINGS = 6,
    CODEG_EUI_OP_SET_AGENT_SETTINGS = 7,
    CODEG_EUI_OP_PROBE_AGENT = 8,
} CodegEuiOperation;

typedef enum CodegEuiCompletionStatus {
    CODEG_EUI_COMPLETION_OK = 0,
    CODEG_EUI_COMPLETION_ERROR = 1,
    CODEG_EUI_COMPLETION_STALE = 2,
    CODEG_EUI_COMPLETION_CANCELLED = 3,
} CodegEuiCompletionStatus;

typedef struct CodegEuiSlice {
    const uint8_t* ptr;
    size_t len;
} CodegEuiSlice;

typedef struct CodegEuiSessionSummary {
    int32_t conversation_id;
    uint32_t reserved;
    CodegEuiSlice title;
    CodegEuiSlice agent;
    int64_t updated_at_ms;
} CodegEuiSessionSummary;

typedef struct CodegEuiCompletion {
    uint64_t request_id;
    uint32_t op;
    uint32_t status;
    CodegEuiSlice result_payload;
    CodegEuiSlice error;
} CodegEuiCompletion;

typedef struct CodegEuiFrame {
    uint32_t api_version;
    uint32_t lifecycle_state;
    uint64_t generation;
    uint64_t selection_epoch;
    const CodegEuiSessionSummary* sessions;
    size_t sessions_len;
    CodegEuiSlice connection_id;
    uint64_t event_seq;
    CodegEuiSlice transcript_json;
    CodegEuiSlice live_assistant;
    uint8_t stream_active;
    uint8_t needs_resync;
    uint8_t shutdown_ready;
    uint8_t reserved[5];
    CodegEuiSlice error_strip;
    const CodegEuiCompletion* completions;
    size_t completions_len;
    uint64_t t0_ns;
    uint64_t t_first_token_ns;
    uint64_t t_end_ns;
} CodegEuiFrame;

#if defined(__cplusplus)
extern "C" {
#endif

uint32_t codeg_eui_api_version(void);
int codeg_eui_init(const uint8_t* data_dir_utf8, size_t data_dir_len);
int codeg_eui_poll(CodegEuiFrame* out);
int codeg_eui_begin_shutdown(void);
int codeg_eui_shutdown(void);
int codeg_eui_set_workspace(const uint8_t* path_utf8,
                            size_t path_len,
                            uint64_t* out_request_id);
int codeg_eui_create_session(const uint8_t* agent_utf8,
                             size_t agent_len,
                             uint64_t* out_request_id);
int codeg_eui_select_session(int32_t conversation_id,
                             uint64_t* out_request_id);
int codeg_eui_send_user_message(const uint8_t* text_utf8,
                                size_t text_len,
                                uint64_t* out_request_id);
int codeg_eui_cancel_active_turn(uint64_t* out_request_id);
int codeg_eui_get_agent_settings(const uint8_t* agent_utf8,
                                 size_t agent_len,
                                 uint64_t* out_request_id);
int codeg_eui_set_agent_settings(const uint8_t* agent_utf8,
                                 size_t agent_len,
                                 const uint8_t* json_utf8,
                                 size_t json_len,
                                 uint64_t* out_request_id);
int codeg_eui_probe_agent(const uint8_t* agent_utf8,
                          size_t agent_len,
                          uint64_t* out_request_id);

#if defined(CODEG_EUI_TEST_HOOKS)
int codeg_eui_test_enqueue_blocked(uint64_t* out_request_id);
#endif

#if defined(__cplusplus)
}

static_assert(sizeof(CodegEuiLifecycleState) == 4,
              "CodegEuiLifecycleState ABI drift");
static_assert(sizeof(CodegEuiOperation) == 4,
              "CodegEuiOperation ABI drift");
static_assert(sizeof(CodegEuiCompletionStatus) == 4,
              "CodegEuiCompletionStatus ABI drift");
static_assert(sizeof(CodegEuiSlice) == 16, "CodegEuiSlice ABI drift");
static_assert(alignof(CodegEuiSlice) == 8, "CodegEuiSlice alignment drift");
static_assert(sizeof(CodegEuiSessionSummary) == 48,
              "CodegEuiSessionSummary ABI drift");
static_assert(alignof(CodegEuiSessionSummary) == 8,
              "CodegEuiSessionSummary alignment drift");
static_assert(sizeof(CodegEuiCompletion) == 48,
              "CodegEuiCompletion ABI drift");
static_assert(alignof(CodegEuiCompletion) == 8,
              "CodegEuiCompletion alignment drift");
static_assert(sizeof(CodegEuiFrame) == 160, "CodegEuiFrame ABI drift");
static_assert(alignof(CodegEuiFrame) == 8, "CodegEuiFrame alignment drift");
#elif defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
_Static_assert(sizeof(CodegEuiLifecycleState) == 4,
               "CodegEuiLifecycleState ABI drift");
_Static_assert(sizeof(CodegEuiOperation) == 4,
               "CodegEuiOperation ABI drift");
_Static_assert(sizeof(CodegEuiCompletionStatus) == 4,
               "CodegEuiCompletionStatus ABI drift");
_Static_assert(sizeof(CodegEuiSlice) == 16, "CodegEuiSlice ABI drift");
_Static_assert(_Alignof(CodegEuiSlice) == 8,
               "CodegEuiSlice alignment drift");
_Static_assert(sizeof(CodegEuiSessionSummary) == 48,
               "CodegEuiSessionSummary ABI drift");
_Static_assert(_Alignof(CodegEuiSessionSummary) == 8,
               "CodegEuiSessionSummary alignment drift");
_Static_assert(sizeof(CodegEuiCompletion) == 48,
               "CodegEuiCompletion ABI drift");
_Static_assert(_Alignof(CodegEuiCompletion) == 8,
               "CodegEuiCompletion alignment drift");
_Static_assert(sizeof(CodegEuiFrame) == 160, "CodegEuiFrame ABI drift");
_Static_assert(_Alignof(CodegEuiFrame) == 8,
               "CodegEuiFrame alignment drift");
#endif
