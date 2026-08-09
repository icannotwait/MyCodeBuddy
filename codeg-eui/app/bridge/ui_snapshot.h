#pragma once

#include "codeg_eui_bridge.h"

#include <cstddef>
#include <cstdint>
#include <stdexcept>
#include <string>
#include <vector>

struct UiSessionSummary {
    std::int32_t conversationId{};
    std::string title;
    std::string agent;
    std::int64_t updatedAtMs{};
};

struct UiCompletion {
    std::uint64_t requestId{};
    std::uint32_t op{};
    std::uint32_t status{};
    std::string resultPayload;
    std::string error;
};

struct UiSnapshot {
    std::uint32_t apiVersion{};
    std::uint32_t lifecycleState{};
    std::uint64_t generation{};
    std::uint64_t selectionEpoch{};
    std::vector<UiSessionSummary> sessions;
    std::string connectionId;
    std::uint64_t eventSeq{};
    std::string transcriptJson;
    std::string liveAssistant;
    bool streamActive{};
    bool needsResync{};
    bool shutdownReady{};
    std::string errorStrip;
    std::vector<UiCompletion> completions;
    std::uint64_t t0Ns{};
    std::uint64_t tFirstTokenNs{};
    std::uint64_t tEndNs{};
};

inline std::string copy_slice(CodegEuiSlice slice, const char* field) {
    if (slice.len == 0) {
        return {};
    }
    if (slice.ptr == nullptr) {
        throw std::invalid_argument(std::string(field) + " has null data");
    }
    return {reinterpret_cast<const char*>(slice.ptr), slice.len};
}

inline UiSnapshot copy_frame(const CodegEuiFrame& frame) {
    if (frame.sessions_len > 0 && frame.sessions == nullptr) {
        throw std::invalid_argument("sessions has null data");
    }
    if (frame.completions_len > 0 && frame.completions == nullptr) {
        throw std::invalid_argument("completions has null data");
    }

    UiSnapshot snapshot;
    snapshot.apiVersion = frame.api_version;
    snapshot.lifecycleState = frame.lifecycle_state;
    snapshot.generation = frame.generation;
    snapshot.selectionEpoch = frame.selection_epoch;
    snapshot.sessions.reserve(frame.sessions_len);
    for (std::size_t index = 0; index < frame.sessions_len; ++index) {
        const CodegEuiSessionSummary& session = frame.sessions[index];
        snapshot.sessions.push_back({
            session.conversation_id,
            copy_slice(session.title, "session.title"),
            copy_slice(session.agent, "session.agent"),
            session.updated_at_ms,
        });
    }
    snapshot.connectionId = copy_slice(frame.connection_id, "connection_id");
    snapshot.eventSeq = frame.event_seq;
    snapshot.transcriptJson =
        copy_slice(frame.transcript_json, "transcript_json");
    snapshot.liveAssistant =
        copy_slice(frame.live_assistant, "live_assistant");
    snapshot.streamActive = frame.stream_active != 0;
    snapshot.needsResync = frame.needs_resync != 0;
    snapshot.shutdownReady = frame.shutdown_ready != 0;
    snapshot.errorStrip = copy_slice(frame.error_strip, "error_strip");
    snapshot.completions.reserve(frame.completions_len);
    for (std::size_t index = 0; index < frame.completions_len; ++index) {
        const CodegEuiCompletion& completion = frame.completions[index];
        snapshot.completions.push_back({
            completion.request_id,
            completion.op,
            completion.status,
            copy_slice(completion.result_payload, "completion.result_payload"),
            copy_slice(completion.error, "completion.error"),
        });
    }
    snapshot.t0Ns = frame.t0_ns;
    snapshot.tFirstTokenNs = frame.t_first_token_ns;
    snapshot.tEndNs = frame.t_end_ns;
    return snapshot;
}
