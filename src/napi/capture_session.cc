#include "napi/capture_session.h"

#include "core/capture/wgc_source.h"
#include "core/common/clock.h"
#include "core/common/errors.h"
#include "napi/tsfn.h"

#include <utility>

namespace wincap {

namespace {

// Marshalling payload for a single delivered frame.
struct FramePayload {
    FrameSlot*    slot{nullptr};
    FramePool*    pool{nullptr};
    std::uint32_t width{0};
    std::uint32_t height{0};
    std::uint64_t timestamp_ns{0};
    bool          size_changed{false};
};

struct ErrorPayload {
    std::string component;
    long        hr{0};
    std::string message;
};

// Parse the source spec from the JS options object. For M1 we accept:
//   { source: { kind: 'display', monitorHandle: <BigInt> } }
//   { source: { kind: 'window',  hwnd: <BigInt> } }
struct SourceSpec {
    enum class Kind { Display, Window } kind{Kind::Display};
    HMONITOR monitor{nullptr};
    HWND     hwnd{nullptr};
};

SourceSpec ParseSource(Napi::Env env, Napi::Object opts) {
    if (!opts.Has("source")) {
        throw Napi::TypeError::New(env, "options.source is required");
    }
    Napi::Object src = opts.Get("source").As<Napi::Object>();
    const std::string kind = src.Get("kind").As<Napi::String>();
    SourceSpec out{};
    if (kind == "display") {
        out.kind = SourceSpec::Kind::Display;
        if (!src.Has("monitorHandle")) {
            throw Napi::TypeError::New(env, "display source requires monitorHandle (BigInt)");
        }
        bool lossless = false;
        const uint64_t v = src.Get("monitorHandle").As<Napi::BigInt>().Uint64Value(&lossless);
        out.monitor = reinterpret_cast<HMONITOR>(static_cast<uintptr_t>(v));
    } else if (kind == "window") {
        out.kind = SourceSpec::Kind::Window;
        if (!src.Has("hwnd")) {
            throw Napi::TypeError::New(env, "window source requires hwnd (BigInt)");
        }
        bool lossless = false;
        const uint64_t v = src.Get("hwnd").As<Napi::BigInt>().Uint64Value(&lossless);
        out.hwnd = reinterpret_cast<HWND>(static_cast<uintptr_t>(v));
    } else {
        throw Napi::TypeError::New(env, "source.kind must be 'display' or 'window'");
    }
    return out;
}

} // namespace

Napi::Object CaptureSession::Init(Napi::Env env, Napi::Object exports) {
    Napi::Function ctor = DefineClass(env, "CaptureSession", {
        InstanceMethod("start",    &CaptureSession::Start),
        InstanceMethod("stop",     &CaptureSession::Stop),
        InstanceMethod("getStats", &CaptureSession::GetStats),
    });
    exports.Set("CaptureSession", ctor);
    return exports;
}

CaptureSession::CaptureSession(const Napi::CallbackInfo& info)
    : Napi::ObjectWrap<CaptureSession>(info) {
    Napi::Env env = info.Env();
    if (info.Length() < 1 || !info[0].IsObject()) {
        throw Napi::TypeError::New(env, "CaptureSession(options, onFrame, onError)");
    }
    if (info.Length() < 2 || !info[1].IsFunction()) {
        throw Napi::TypeError::New(env, "second arg must be onFrame callback");
    }
    if (info.Length() < 3 || !info[2].IsFunction()) {
        throw Napi::TypeError::New(env, "third arg must be onError callback");
    }

    Napi::Object opts = info[0].As<Napi::Object>();
    SourceSpec spec = ParseSource(env, opts);

    Clock::Init();

    try {
        device_.Create(LUID{0, 0});

        // Pool textures: BGRA8 staging-friendly, will be sized on first frame
        // by WgcSource via RecreateFramePool. Pre-create with 1x1 placeholder.
        D3D11_TEXTURE2D_DESC desc{};
        desc.Width            = 1;
        desc.Height           = 1;
        desc.MipLevels        = 1;
        desc.ArraySize        = 1;
        desc.Format           = DXGI_FORMAT_B8G8R8A8_UNORM;
        desc.SampleDesc.Count = 1;
        desc.Usage            = D3D11_USAGE_DEFAULT;
        desc.BindFlags        = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
        // NOTE: M1 placeholder pool. The capture source recreates the WGC
        // FramePool on size change; a follow-up will resize our slots too.
        if (!pool_.Init(device_.Device(), 4, desc, /*shared*/ false)) {
            throw Napi::Error::New(env, "frame pool init failed");
        }

        WgcOptions wgc_opts{};
        if (opts.Has("includeCursor")) wgc_opts.include_cursor = opts.Get("includeCursor").ToBoolean();
        if (opts.Has("borderRequired")) wgc_opts.border_required = opts.Get("borderRequired").ToBoolean();

        auto wgc = std::make_unique<WgcSource>(device_, pool_, wgc_opts);
        if (spec.kind == SourceSpec::Kind::Display) {
            wgc->InitForMonitor(spec.monitor);
        } else {
            wgc->InitForWindow(spec.hwnd);
        }
        source_ = std::move(wgc);
    } catch (HrError const& e) {
        throw Napi::Error::New(env,
            std::string("wincap: ") + e.component() + " " + HrError::FormatHr(e.hr()) + " " + e.what());
    }

    on_frame_tsfn_ = Napi::ThreadSafeFunction::New(
        env, info[1].As<Napi::Function>(), "wincap.onFrame", /*max_queue*/ 4, /*threads*/ 1);
    on_error_tsfn_ = Napi::ThreadSafeFunction::New(
        env, info[2].As<Napi::Function>(), "wincap.onError", /*max_queue*/ 8, /*threads*/ 1);
}

CaptureSession::~CaptureSession() {
    if (running_.load()) {
        if (source_) source_->Stop();
        running_.store(false);
    }
    if (on_frame_tsfn_) on_frame_tsfn_.Release();
    if (on_error_tsfn_) on_error_tsfn_.Release();
    pool_.Shutdown();
    device_.Reset();
}

Napi::Value CaptureSession::Start(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (running_.exchange(true)) return env.Undefined();
    try {
        source_->Start(
            [this](const CapturedFrame& f) { DispatchFrame(f); },
            [this](const char* c, long hr, const char* m) { DispatchError(c, hr, m); });
    } catch (HrError const& e) {
        running_.store(false);
        throw Napi::Error::New(env,
            std::string("wincap: ") + e.component() + " " + HrError::FormatHr(e.hr()) + " " + e.what());
    }
    return env.Undefined();
}

Napi::Value CaptureSession::Stop(const Napi::CallbackInfo& info) {
    if (!running_.exchange(false)) return info.Env().Undefined();
    if (source_) source_->Stop();
    return info.Env().Undefined();
}

Napi::Value CaptureSession::GetStats(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    Napi::Object out = Napi::Object::New(env);
    out.Set("deliveredFrames", Napi::BigInt::New(env, delivered_frames_.load()));
    out.Set("droppedFrames",   Napi::BigInt::New(env, dropped_frames_.load()));
    return out;
}

void CaptureSession::DispatchFrame(const CapturedFrame& frame) {
    auto* payload = new FramePayload{
        frame.slot, &pool_, frame.width, frame.height, frame.timestamp_ns, frame.size_changed
    };

    const napi_status s = on_frame_tsfn_.NonBlockingCall(payload,
        [](Napi::Env env, Napi::Function jsCb, FramePayload* p) {
            // Build VideoFrame object on JS thread.
            Napi::HandleScope scope(env);
            Napi::Object frame = Napi::Object::New(env);
            frame.Set("timestampNs", Napi::BigInt::New(env, p->timestamp_ns));
            frame.Set("width",       Napi::Number::New(env, p->width));
            frame.Set("height",      Napi::Number::New(env, p->height));
            frame.Set("format",      Napi::String::New(env, "bgra8"));
            frame.Set("sizeChanged", Napi::Boolean::New(env, p->size_changed));

            // M1: no zero-copy ArrayBuffer yet — we expose only metadata
            // plus the pool slot index for native consumers. CPU readback
            // path lands in M1.5; shared NT handle in M3.
            FramePool* pool = p->pool;
            FrameSlot* slot = p->slot;
            auto release = Napi::Function::New(env,
                [pool, slot](const Napi::CallbackInfo&) {
                    pool->Release(slot);
                });
            frame.Set("release", release);

            jsCb.Call({ frame });
            delete p;
        });

    if (s == napi_ok) {
        delivered_frames_.fetch_add(1, std::memory_order_relaxed);
    } else {
        // Queue full or shutting down: drop the frame and return its slot.
        dropped_frames_.fetch_add(1, std::memory_order_relaxed);
        pool_.Release(payload->slot);
        delete payload;
    }
}

void CaptureSession::DispatchError(const char* component, long hr, const char* msg) {
    auto* payload = new ErrorPayload{component, hr, msg};
    const napi_status s = on_error_tsfn_.NonBlockingCall(payload,
        [](Napi::Env env, Napi::Function jsCb, ErrorPayload* p) {
            Napi::HandleScope scope(env);
            Napi::Object err = Napi::Object::New(env);
            err.Set("component", Napi::String::New(env, p->component));
            err.Set("hresult",   Napi::Number::New(env, static_cast<double>(p->hr)));
            err.Set("message",   Napi::String::New(env, p->message));
            jsCb.Call({ err });
            delete p;
        });
    if (s != napi_ok) delete payload;
}

} // namespace wincap
