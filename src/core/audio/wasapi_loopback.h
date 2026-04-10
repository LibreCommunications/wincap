// WASAPI loopback capture. Two modes:
//   - System: default render endpoint loopback (Vista+), event-driven.
//   - Process: per-PID loopback via ActivateAudioInterfaceAsync with
//     AUDIOCLIENT_ACTIVATION_PARAMS{ PROCESS_LOOPBACK } (Win11 22000+),
//     including process tree. Activation is async — we wait on it.
//
// In both cases the hot loop runs on a dedicated thread with MMCSS
// "Pro Audio". A small ring of pre-allocated float32 buffers is used to
// hand frames to the consumer without per-frame allocation.
#pragma once

#include "core/audio/iaudio_source.h"
#include "core/audio/format.h"

#include <atomic>
#include <cstdint>
#include <thread>
#include <vector>

// windows.h must precede the WASAPI / mmreg headers (they depend on
// windef macros like NEAR/FAR/WORD).
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <wrl/client.h>
#include <audioclient.h>
#include <mmdeviceapi.h>

namespace wincap {

enum class LoopbackMode {
    SystemDefault,
    ProcessTree,
};

struct WasapiLoopbackOptions {
    LoopbackMode  mode{LoopbackMode::SystemDefault};
    std::uint32_t target_pid{0};            // for ProcessTree
    bool          include_tree{true};
};

class WasapiLoopback final : public IAudioSource {
public:
    explicit WasapiLoopback(WasapiLoopbackOptions opts);
    ~WasapiLoopback() override;

    void Start(AudioCallback cb, AudioErrorCallback err) override;
    void Stop() override;

    const AudioFormat& Format() const noexcept { return format_; }

private:
    void ThreadMain();
    void Activate();          // sync (system) or async-then-wait (process)
    void Initialize();        // IAudioClient::Initialize, get capture client
    void ReleaseClient() noexcept;

    static void ReleaseChunk(void* opaque) noexcept;

    struct PoolBuffer {
        std::vector<float>          data;
        std::atomic<std::uint32_t>  in_use{0};
        WasapiLoopback*             owner{nullptr};
    };
    PoolBuffer* AcquireBuffer(std::size_t needed_floats) noexcept;

    WasapiLoopbackOptions opts_;
    AudioFormat           format_{};

    Microsoft::WRL::ComPtr<IAudioClient>        client_;
    Microsoft::WRL::ComPtr<IAudioCaptureClient> capture_;
    HANDLE                                      event_{nullptr};

    std::thread                  thread_;
    std::atomic<bool>            running_{false};
    HANDLE                       stop_event_{nullptr};

    AudioCallback      cb_;
    AudioErrorCallback err_cb_;

    static constexpr std::size_t kPoolSize = 16;
    std::vector<PoolBuffer> pool_{};
};

} // namespace wincap
