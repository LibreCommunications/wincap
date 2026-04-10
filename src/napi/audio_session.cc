#include "napi/audio_session.h"

#include "core/common/errors.h"

namespace wincap {

namespace {

struct ChunkPayload {
    const float*  data{nullptr};
    std::uint32_t frame_count{0};
    std::uint32_t channels{0};
    std::uint32_t sample_rate{0};
    std::uint64_t timestamp_ns{0};
    bool          silent{false};
    bool          discontinuity{false};
    void (*release_fn)(void*) {nullptr};
    void* release_opaque{nullptr};
};

struct ErrorPayload {
    std::string component;
    long        hr{0};
    std::string message;
};

} // namespace

Napi::Object AudioSession::Init(Napi::Env env, Napi::Object exports) {
    Napi::Function ctor = DefineClass(env, "AudioSession", {
        InstanceMethod("start",    &AudioSession::Start),
        InstanceMethod("stop",     &AudioSession::Stop),
        InstanceMethod("getStats", &AudioSession::GetStats),
    });
    exports.Set("AudioSession", ctor);
    return exports;
}

AudioSession::AudioSession(const Napi::CallbackInfo& info)
    : Napi::ObjectWrap<AudioSession>(info) {
    Napi::Env env = info.Env();
    if (info.Length() < 3 || !info[0].IsObject() ||
        !info[1].IsFunction() || !info[2].IsFunction()) {
        throw Napi::TypeError::New(env,
            "AudioSession(options, onChunk, onError)");
    }

    Napi::Object opts = info[0].As<Napi::Object>();
    WasapiLoopbackOptions wopts{};

    const std::string mode = opts.Get("mode").ToString();
    if (mode == "systemLoopback") {
        wopts.mode = LoopbackMode::SystemDefault;
    } else if (mode == "processLoopback") {
        wopts.mode = LoopbackMode::ProcessTree;
        if (!opts.Has("pid")) {
            throw Napi::TypeError::New(env, "processLoopback requires opts.pid");
        }
        wopts.target_pid = opts.Get("pid").As<Napi::Number>().Uint32Value();
        wopts.include_tree = !opts.Has("includeTree") || opts.Get("includeTree").ToBoolean();
    } else {
        throw Napi::TypeError::New(env, "mode must be 'systemLoopback' or 'processLoopback'");
    }

    source_ = std::make_unique<WasapiLoopback>(wopts);

    on_chunk_tsfn_ = Napi::ThreadSafeFunction::New(
        env, info[1].As<Napi::Function>(), "wincap.onAudioChunk", 32, 1);
    on_error_tsfn_ = Napi::ThreadSafeFunction::New(
        env, info[2].As<Napi::Function>(), "wincap.onAudioError", 8, 1);
}

AudioSession::~AudioSession() {
    if (running_.load()) {
        if (source_) source_->Stop();
        running_.store(false);
    }
    if (on_chunk_tsfn_) on_chunk_tsfn_.Release();
    if (on_error_tsfn_) on_error_tsfn_.Release();
}

Napi::Value AudioSession::Start(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (running_.exchange(true)) return env.Undefined();
    source_->Start(
        [this](const AudioChunk& c) { DispatchChunk(c); },
        [this](const char* c, long hr, const char* m) { DispatchError(c, hr, m); });
    return env.Undefined();
}

Napi::Value AudioSession::Stop(const Napi::CallbackInfo& info) {
    if (!running_.exchange(false)) return info.Env().Undefined();
    if (source_) source_->Stop();
    return info.Env().Undefined();
}

Napi::Value AudioSession::GetStats(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    Napi::Object out = Napi::Object::New(env);
    out.Set("deliveredChunks", Napi::BigInt::New(env, delivered_chunks_.load()));
    out.Set("droppedChunks",   Napi::BigInt::New(env, dropped_chunks_.load()));
    out.Set("discontinuities", Napi::BigInt::New(env, discontinuities_.load()));
    return out;
}

void AudioSession::DispatchChunk(const AudioChunk& chunk) {
    if (chunk.discontinuity) discontinuities_.fetch_add(1, std::memory_order_relaxed);

    auto* p = new ChunkPayload{
        chunk.data, chunk.frame_count, chunk.channels, chunk.sample_rate,
        chunk.timestamp_ns, chunk.silent, chunk.discontinuity,
        chunk.release_fn, chunk.release_opaque
    };

    const napi_status s = on_chunk_tsfn_.NonBlockingCall(p,
        [](Napi::Env env, Napi::Function jsCb, ChunkPayload* p) {
            Napi::HandleScope scope(env);
            const std::size_t bytes =
                static_cast<std::size_t>(p->frame_count) * p->channels * sizeof(float);

            // Zero-copy ArrayBuffer pointing at the pool buffer; the
            // finalizer returns the slot to the native pool.
            auto release_fn     = p->release_fn;
            auto release_opaque = p->release_opaque;
            Napi::ArrayBuffer ab = Napi::ArrayBuffer::New(
                env,
                const_cast<float*>(p->data),
                bytes,
                [](Napi::Env, void* /*data*/, void* hint) {
                    auto* fn_and_op = static_cast<std::pair<void(*)(void*), void*>*>(hint);
                    fn_and_op->first(fn_and_op->second);
                    delete fn_and_op;
                },
                new std::pair<void(*)(void*), void*>(release_fn, release_opaque));

            Napi::Object o = Napi::Object::New(env);
            o.Set("timestampNs",   Napi::BigInt::New(env, p->timestamp_ns));
            o.Set("frameCount",    Napi::Number::New(env, p->frame_count));
            o.Set("sampleRate",    Napi::Number::New(env, p->sample_rate));
            o.Set("channels",      Napi::Number::New(env, p->channels));
            o.Set("format",        Napi::String::New(env, "float32"));
            o.Set("silent",        Napi::Boolean::New(env, p->silent));
            o.Set("discontinuity", Napi::Boolean::New(env, p->discontinuity));
            o.Set("data",          ab);

            jsCb.Call({ o });
            delete p;
        });

    if (s == napi_ok) {
        delivered_chunks_.fetch_add(1, std::memory_order_relaxed);
    } else {
        dropped_chunks_.fetch_add(1, std::memory_order_relaxed);
        if (p->release_fn) p->release_fn(p->release_opaque);
        delete p;
    }
}

void AudioSession::DispatchError(const char* component, long hr, const char* msg) {
    auto* p = new ErrorPayload{component, hr, msg};
    const napi_status s = on_error_tsfn_.NonBlockingCall(p,
        [](Napi::Env env, Napi::Function jsCb, ErrorPayload* p) {
            Napi::HandleScope scope(env);
            Napi::Object err = Napi::Object::New(env);
            err.Set("component", Napi::String::New(env, p->component));
            err.Set("hresult",   Napi::Number::New(env, static_cast<double>(p->hr)));
            err.Set("message",   Napi::String::New(env, p->message));
            jsCb.Call({ err });
            delete p;
        });
    if (s != napi_ok) delete p;
}

} // namespace wincap
