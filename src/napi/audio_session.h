// JS-facing AudioSession. Owns a WasapiLoopback (system or process mode)
// and forwards each chunk to JS via a ThreadSafeFunction. The JS-side
// ArrayBuffer is created with an external backing pointer + finalizer
// that returns the pool slot to the native source — zero-copy.
#pragma once

#include "core/audio/wasapi_loopback.h"

#include <atomic>
#include <memory>
#include <napi.h>

namespace wincap {

class AudioSession : public Napi::ObjectWrap<AudioSession> {
public:
    static Napi::Object Init(Napi::Env env, Napi::Object exports);
    explicit AudioSession(const Napi::CallbackInfo& info);
    ~AudioSession() override;

private:
    Napi::Value Start(const Napi::CallbackInfo& info);
    Napi::Value Stop(const Napi::CallbackInfo& info);
    Napi::Value GetStats(const Napi::CallbackInfo& info);

    void DispatchChunk(const AudioChunk& chunk);
    void DispatchError(const char* component, long hr, const char* msg);

    std::unique_ptr<WasapiLoopback> source_;

    Napi::ThreadSafeFunction on_chunk_tsfn_;
    Napi::ThreadSafeFunction on_error_tsfn_;

    std::atomic<bool>     running_{false};
    std::atomic<uint64_t> delivered_chunks_{0};
    std::atomic<uint64_t> dropped_chunks_{0};
    std::atomic<uint64_t> discontinuities_{0};
};

} // namespace wincap
