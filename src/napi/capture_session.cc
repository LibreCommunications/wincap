#include "napi/capture_session.h"

#include "core/capture/wgc_source.h"
#include "core/common/clock.h"
#include "core/common/errors.h"
#include "core/encoder/mf_encoder.h"
#include "napi/tsfn.h"

#include <cstring>
#include <utility>
#include <vector>

#include <winrt/Windows.Graphics.DirectX.h>

namespace wincap {

namespace {

struct FramePayload {
    FrameSlot*    slot{nullptr};
    FramePool*    pool{nullptr};
    std::uint32_t width{0};
    std::uint32_t height{0};
    std::uint64_t timestamp_ns{0};
    bool          size_changed{false};
};

struct EncodedPayload {
    std::vector<std::uint8_t> data;
    std::uint64_t             timestamp_ns{0};
    bool                      keyframe{false};
};

struct ErrorPayload {
    std::string component;
    long        hr{0};
    std::string message;
};

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

EncoderConfig ParseEncoderConfig(Napi::Env env, Napi::Object delivery) {
    EncoderConfig cfg{};
    cfg.codec = VideoCodec::H264;
    if (delivery.Has("codec")) {
        const std::string c = delivery.Get("codec").As<Napi::String>();
        if      (c == "h264") cfg.codec = VideoCodec::H264;
        else if (c == "hevc") cfg.codec = VideoCodec::HEVC;
        else if (c == "av1")  cfg.codec = VideoCodec::AV1;
        else throw Napi::TypeError::New(env, "delivery.codec must be h264|hevc|av1");
    }
    cfg.bitrate_bps = delivery.Has("bitrateBps")
        ? delivery.Get("bitrateBps").As<Napi::Number>().Uint32Value()
        : 6'000'000u;
    cfg.fps = delivery.Has("fps")
        ? delivery.Get("fps").As<Napi::Number>().Uint32Value()
        : 60u;
    cfg.keyframe_interval_ms = delivery.Has("keyframeIntervalMs")
        ? delivery.Get("keyframeIntervalMs").As<Napi::Number>().Uint32Value()
        : 2000u;
    if (delivery.Has("hdr10"))        cfg.hdr10 = delivery.Get("hdr10").ToBoolean();
    if (delivery.Has("ltrCount"))     cfg.ltr_count = delivery.Get("ltrCount").As<Napi::Number>().Uint32Value();
    if (delivery.Has("intraRefresh")) cfg.intra_refresh = delivery.Get("intraRefresh").ToBoolean();
    if (delivery.Has("intraRefreshPeriod"))
        cfg.intra_refresh_period = delivery.Get("intraRefreshPeriod").As<Napi::Number>().Uint32Value();
    if (delivery.Has("roiEnabled"))   cfg.roi_enabled = delivery.Get("roiEnabled").ToBoolean();
    return cfg;
}

} // namespace

Napi::Object CaptureSession::Init(Napi::Env env, Napi::Object exports) {
    Napi::Function ctor = DefineClass(env, "CaptureSession", {
        InstanceMethod("start",           &CaptureSession::Start),
        InstanceMethod("stop",            &CaptureSession::Stop),
        InstanceMethod("getStats",        &CaptureSession::GetStats),
        InstanceMethod("requestKeyframe", &CaptureSession::RequestKeyframe),
        InstanceMethod("setBitrate",      &CaptureSession::SetBitrate),
    });
    exports.Set("CaptureSession", ctor);
    return exports;
}

CaptureSession::CaptureSession(const Napi::CallbackInfo& info)
    : Napi::ObjectWrap<CaptureSession>(info) {
    Napi::Env env = info.Env();
    if (info.Length() < 4 || !info[0].IsObject() ||
        !info[1].IsFunction() || !info[2].IsFunction() || !info[3].IsFunction()) {
        throw Napi::TypeError::New(env,
            "CaptureSession(options, onFrame, onEncoded, onError)");
    }

    Napi::Object opts = info[0].As<Napi::Object>();
    SourceSpec spec = ParseSource(env, opts);

    // Delivery mode: 'raw' (metadata only) | 'cpu' (BGRA8 ArrayBuffer
    // via staging readback) | 'encoded' (H.264/HEVC/AV1 NAL units).
    if (opts.Has("delivery")) {
        Napi::Object delivery = opts.Get("delivery").As<Napi::Object>();
        const std::string t = delivery.Get("type").As<Napi::String>();
        if (t == "encoded") {
            delivery_ = DeliveryMode::Encoded;
            enc_cfg_  = ParseEncoderConfig(env, delivery);
        } else if (t == "cpu") {
            delivery_ = DeliveryMode::Cpu;
        } else if (t != "raw") {
            throw Napi::TypeError::New(env, "delivery.type must be 'raw' | 'cpu' | 'encoded'");
        }
    }

    Clock::Init();

    try {
        device_.Create(LUID{0, 0});

        // HDR-aware pixel format for the BGRA-equivalent capture pool.
        // In HDR10 mode WGC produces R16G16B16A16Float (scRGB).
        const bool hdr_capture =
            (delivery_ == DeliveryMode::Encoded && enc_cfg_.hdr10);

        D3D11_TEXTURE2D_DESC desc{};
        desc.Width            = 1;
        desc.Height           = 1;
        desc.MipLevels        = 1;
        desc.ArraySize        = 1;
        desc.Format           = hdr_capture ? DXGI_FORMAT_R16G16B16A16_FLOAT
                                            : DXGI_FORMAT_B8G8R8A8_UNORM;
        desc.SampleDesc.Count = 1;
        desc.Usage            = D3D11_USAGE_DEFAULT;
        desc.BindFlags        = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
        if (!pool_.Init(device_.Device(), 4, desc, /*shared*/ false)) {
            throw Napi::Error::New(env, "frame pool init failed");
        }

        WgcOptions wgc_opts{};
        if (opts.Has("includeCursor"))  wgc_opts.include_cursor  = opts.Get("includeCursor").ToBoolean();
        if (opts.Has("borderRequired")) wgc_opts.border_required = opts.Get("borderRequired").ToBoolean();
        if (hdr_capture) {
            wgc_opts.pixel_format =
                winrt::Windows::Graphics::DirectX::DirectXPixelFormat::R16G16B16A16Float;
        }

        auto wgc = std::make_unique<WgcSource>(device_, pool_, wgc_opts);
        if (spec.kind == SourceSpec::Kind::Display) wgc->InitForMonitor(spec.monitor);
        else                                        wgc->InitForWindow(spec.hwnd);
        source_ = std::move(wgc);
    } catch (HrError const& e) {
        throw Napi::Error::New(env,
            std::string("wincap: ") + e.component() + " " + HrError::FormatHr(e.hr()) + " " + e.what());
    }

    on_frame_tsfn_   = Napi::ThreadSafeFunction::New(env, info[1].As<Napi::Function>(),
                          "wincap.onFrame",   4, 1);
    on_encoded_tsfn_ = Napi::ThreadSafeFunction::New(env, info[2].As<Napi::Function>(),
                          "wincap.onEncoded", 16, 1);
    on_error_tsfn_   = Napi::ThreadSafeFunction::New(env, info[3].As<Napi::Function>(),
                          "wincap.onError",   8, 1);
}

CaptureSession::~CaptureSession() {
    if (running_.load()) {
        if (source_) source_->Stop();
        if (encoder_) encoder_->Stop();
        running_.store(false);
    }
    if (on_frame_tsfn_)   on_frame_tsfn_.Release();
    if (on_encoded_tsfn_) on_encoded_tsfn_.Release();
    if (on_error_tsfn_)   on_error_tsfn_.Release();
    encoder_.reset();
    color_.reset();
    pool_.Shutdown();
    device_.Reset();
}

void CaptureSession::EnsureEncoderInitialized(std::uint32_t width, std::uint32_t height) {
    if (encoder_ && enc_width_ == width && enc_height_ == height) return;

    // (Re)initialise on first frame or on size change.
    if (encoder_) { encoder_->Stop(); encoder_.reset(); }
    color_.reset();

    color_ = std::make_unique<VideoProcessor>();
    const DXGI_FORMAT out_fmt = enc_cfg_.hdr10 ? DXGI_FORMAT_P010 : DXGI_FORMAT_NV12;
    const auto cs = enc_cfg_.hdr10 ? VideoProcessor::ColorSpace::Rec2020Pq
                                   : VideoProcessor::ColorSpace::Rec709Sdr;
    color_->Init(device_.Device(), device_.Context(), width, height, out_fmt, cs);

    EncoderConfig cfg = enc_cfg_;
    cfg.width  = width;
    cfg.height = height;

    auto enc = std::make_unique<MfEncoder>();
    enc->Initialize(device_.Device(), cfg);
    enc->Start(
        [this](const EncodedAccessUnit& au) { OnEncodedOutput(au); },
        [this](const char* c, long hr, const char* m) { DispatchError(c, hr, m); });
    encoder_   = std::move(enc);
    enc_width_  = width;
    enc_height_ = height;
}

Napi::Value CaptureSession::Start(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (running_.exchange(true)) return env.Undefined();
    try {
        auto err_cb = [this](const char* c, long hr, const char* m) { DispatchError(c, hr, m); };
        if (delivery_ == DeliveryMode::Raw) {
            source_->Start([this](const CapturedFrame& f) { DispatchRawFrame(f); }, err_cb);
        } else if (delivery_ == DeliveryMode::Cpu) {
            source_->Start([this](const CapturedFrame& f) { DispatchCpuFrame(f); }, err_cb);
        } else {
            source_->Start([this](const CapturedFrame& f) { DispatchEncodedFrame(f); }, err_cb);
        }
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
    if (encoder_) encoder_->Stop();
    return info.Env().Undefined();
}

Napi::Value CaptureSession::GetStats(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    Napi::Object out = Napi::Object::New(env);
    out.Set("deliveredFrames", Napi::BigInt::New(env, delivered_frames_.load()));
    out.Set("droppedFrames",   Napi::BigInt::New(env, dropped_frames_.load()));
    out.Set("encodedUnits",    Napi::BigInt::New(env, encoded_units_.load()));
    return out;
}

Napi::Value CaptureSession::RequestKeyframe(const Napi::CallbackInfo& info) {
    if (encoder_) encoder_->RequestKeyframe();
    return info.Env().Undefined();
}

Napi::Value CaptureSession::SetBitrate(const Napi::CallbackInfo& info) {
    Napi::Env env = info.Env();
    if (info.Length() < 1 || !info[0].IsNumber()) {
        throw Napi::TypeError::New(env, "setBitrate(bps: number)");
    }
    if (encoder_) encoder_->SetBitrate(info[0].As<Napi::Number>().Uint32Value());
    return env.Undefined();
}

void CaptureSession::DispatchRawFrame(const CapturedFrame& frame) {
    auto* payload = new FramePayload{
        frame.slot, &pool_, frame.width, frame.height, frame.timestamp_ns, frame.size_changed
    };

    const napi_status s = on_frame_tsfn_.NonBlockingCall(payload,
        [](Napi::Env env, Napi::Function jsCb, FramePayload* p) {
            Napi::HandleScope scope(env);
            Napi::Object frame = Napi::Object::New(env);
            frame.Set("timestampNs", Napi::BigInt::New(env, p->timestamp_ns));
            frame.Set("width",       Napi::Number::New(env, p->width));
            frame.Set("height",      Napi::Number::New(env, p->height));
            frame.Set("format",      Napi::String::New(env, "bgra8"));
            frame.Set("sizeChanged", Napi::Boolean::New(env, p->size_changed));

            FramePool* pool = p->pool;
            FrameSlot* slot = p->slot;
            auto release = Napi::Function::New(env,
                [pool, slot](const Napi::CallbackInfo&) { pool->Release(slot); });
            frame.Set("release", release);

            jsCb.Call({ frame });
            delete p;
        });

    if (s == napi_ok) {
        delivered_frames_.fetch_add(1, std::memory_order_relaxed);
    } else {
        dropped_frames_.fetch_add(1, std::memory_order_relaxed);
        pool_.Release(payload->slot);
        delete payload;
    }
}

void CaptureSession::EnsureStagingInitialized(std::uint32_t width, std::uint32_t height) {
    if (staging_w_ == width && staging_h_ == height && staging_[0].tex) return;

    // (Re)create staging textures and fences.
    for (auto& s : staging_) {
        s.tex.Reset();
        s.fence.Reset();
        s.in_js.store(0, std::memory_order_release);
    }

    D3D11_TEXTURE2D_DESC desc{};
    desc.Width            = width;
    desc.Height           = height;
    desc.MipLevels        = 1;
    desc.ArraySize        = 1;
    desc.Format           = DXGI_FORMAT_B8G8R8A8_UNORM;
    desc.SampleDesc.Count = 1;
    desc.Usage            = D3D11_USAGE_STAGING;
    desc.CPUAccessFlags   = D3D11_CPU_ACCESS_READ;

    for (std::uint32_t i = 0; i < kStagingCount; ++i) {
        staging_[i].index  = i;
        WINCAP_THROW_IF_FAILED("staging",
            device_.Device()->CreateTexture2D(&desc, nullptr, staging_[i].tex.GetAddressOf()));
        D3D11_QUERY_DESC qd{};
        qd.Query = D3D11_QUERY_EVENT;
        WINCAP_THROW_IF_FAILED("staging",
            device_.Device()->CreateQuery(&qd, staging_[i].fence.GetAddressOf()));
    }

    staging_w_         = width;
    staging_h_         = height;
    staging_write_idx_ = 0;
}

void CaptureSession::DispatchCpuFrame(const CapturedFrame& frame) {
    try {
        EnsureStagingInitialized(frame.width, frame.height);
    } catch (HrError const& e) {
        pool_.Release(frame.slot);
        DispatchError(e.component(), e.hr(), e.what());
        return;
    }

    // Pick the next staging slot. Skip slots still held by JS — bounded
    // by kStagingCount, so this is at most a few comparisons.
    std::uint32_t tries = 0;
    StagingSlot* slot = nullptr;
    while (tries < kStagingCount) {
        StagingSlot* candidate = &staging_[staging_write_idx_];
        staging_write_idx_ = (staging_write_idx_ + 1) % kStagingCount;
        if (candidate->in_js.load(std::memory_order_acquire) == 0) {
            slot = candidate;
            break;
        }
        ++tries;
    }
    if (!slot) {
        // All staging slots in flight to JS — drop this frame.
        dropped_frames_.fetch_add(1, std::memory_order_relaxed);
        pool_.Release(frame.slot);
        return;
    }

    auto* ctx = device_.Context();
    ctx->CopyResource(slot->tex.Get(), frame.slot->texture.Get());
    ctx->End(slot->fence.Get());

    // Wait for the fence (small spin — typical wait is < 1 ms on modern
    // GPUs because the BGRA copy is a tiny operation). For sub-ms target
    // this should be moved to an N-buffered "drain previous slot" model.
    while (true) {
        BOOL done = FALSE;
        HRESULT hr = ctx->GetData(slot->fence.Get(), &done, sizeof(done), 0);
        if (hr == S_OK && done) break;
        if (FAILED(hr)) {
            dropped_frames_.fetch_add(1, std::memory_order_relaxed);
            pool_.Release(frame.slot);
            return;
        }
        ::SwitchToThread();
    }

    D3D11_MAPPED_SUBRESOURCE mapped{};
    HRESULT hr = ctx->Map(slot->tex.Get(), 0, D3D11_MAP_READ, 0, &mapped);
    if (FAILED(hr)) {
        dropped_frames_.fetch_add(1, std::memory_order_relaxed);
        pool_.Release(frame.slot);
        return;
    }

    slot->in_js.store(1, std::memory_order_release);
    slot->width        = frame.width;
    slot->height       = frame.height;
    slot->timestamp_ns = frame.timestamp_ns;

    // BGRA slot can go back to the WGC pool — its content has been
    // copied into staging.
    pool_.Release(frame.slot);

    struct CpuPayload {
        StagingSlot*     slot;
        CaptureSession*  self;
        const std::uint8_t* data;
        std::size_t      bytes;
        std::uint32_t    stride;
        std::uint32_t    width;
        std::uint32_t    height;
        std::uint64_t    timestamp_ns;
    };
    auto* p = new CpuPayload{
        slot, this,
        static_cast<const std::uint8_t*>(mapped.pData),
        static_cast<std::size_t>(mapped.RowPitch) * frame.height,
        mapped.RowPitch, frame.width, frame.height, frame.timestamp_ns
    };

    const napi_status s = on_frame_tsfn_.NonBlockingCall(p,
        [](Napi::Env env, Napi::Function jsCb, CpuPayload* p) {
            Napi::HandleScope scope(env);
            // Zero-copy ArrayBuffer over the mapped staging memory; the
            // finalizer Unmaps and releases the slot back to rotation.
            Napi::ArrayBuffer ab = Napi::ArrayBuffer::New(
                env,
                const_cast<std::uint8_t*>(p->data),
                p->bytes,
                [](Napi::Env, void*, void* hint) {
                    auto* hp = static_cast<CpuPayload*>(hint);
                    hp->self->device_.Context()->Unmap(hp->slot->tex.Get(), 0);
                    hp->slot->in_js.store(0, std::memory_order_release);
                    delete hp;
                },
                p);

            Napi::Object frame = Napi::Object::New(env);
            frame.Set("timestampNs", Napi::BigInt::New(env, p->timestamp_ns));
            frame.Set("width",       Napi::Number::New(env, p->width));
            frame.Set("height",      Napi::Number::New(env, p->height));
            frame.Set("stride",      Napi::Number::New(env, p->stride));
            frame.Set("format",      Napi::String::New(env, "bgra8"));
            frame.Set("data",        ab);
            jsCb.Call({ frame });
        });

    if (s == napi_ok) {
        delivered_frames_.fetch_add(1, std::memory_order_relaxed);
    } else {
        // TSFN full / shutting down: synchronously unmap and release.
        ctx->Unmap(slot->tex.Get(), 0);
        slot->in_js.store(0, std::memory_order_release);
        dropped_frames_.fetch_add(1, std::memory_order_relaxed);
        delete p;
    }
}

void CaptureSession::DispatchEncodedFrame(const CapturedFrame& frame) {
    try {
        EnsureEncoderInitialized(frame.width, frame.height);

        // Allocate a fresh NV12 texture for this frame. The IMFSample
        // wrapping it via MFCreateDXGISurfaceBuffer holds a ComPtr ref,
        // so the texture stays alive until the encoder consumes it.
        D3D11_TEXTURE2D_DESC nv12_desc{};
        nv12_desc.Width            = frame.width;
        nv12_desc.Height           = frame.height;
        nv12_desc.MipLevels        = 1;
        nv12_desc.ArraySize        = 1;
        nv12_desc.Format           = enc_cfg_.hdr10 ? DXGI_FORMAT_P010 : DXGI_FORMAT_NV12;
        nv12_desc.SampleDesc.Count = 1;
        nv12_desc.Usage            = D3D11_USAGE_DEFAULT;
        nv12_desc.BindFlags        = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
        nv12_desc.MiscFlags        = D3D11_RESOURCE_MISC_SHARED; // helps some MFTs

        Microsoft::WRL::ComPtr<ID3D11Texture2D> nv12;
        if (FAILED(device_.Device()->CreateTexture2D(&nv12_desc, nullptr,
                                                     nv12.GetAddressOf()))) {
            dropped_frames_.fetch_add(1, std::memory_order_relaxed);
            pool_.Release(frame.slot);
            return;
        }

        color_->Convert(frame.slot->texture.Get(), nv12.Get());
        encoder_->EncodeFrame(nv12.Get(), frame.timestamp_ns);

        // BGRA slot is no longer needed by us. The GPU command queue is
        // strictly ordered so the convert is guaranteed to read the
        // current contents before WGC writes new contents on reuse.
        pool_.Release(frame.slot);
        delivered_frames_.fetch_add(1, std::memory_order_relaxed);
    } catch (HrError const& e) {
        pool_.Release(frame.slot);
        DispatchError(e.component(), e.hr(), e.what());
    }
}

void CaptureSession::OnEncodedOutput(const EncodedAccessUnit& au) {
    auto* p = new EncodedPayload{};
    p->data.assign(au.data, au.data + au.size);
    p->timestamp_ns = au.timestamp_ns;
    p->keyframe     = au.keyframe;

    const napi_status s = on_encoded_tsfn_.NonBlockingCall(p,
        [](Napi::Env env, Napi::Function jsCb, EncodedPayload* p) {
            Napi::HandleScope scope(env);
            Napi::ArrayBuffer ab = Napi::ArrayBuffer::New(env, p->data.size());
            std::memcpy(ab.Data(), p->data.data(), p->data.size());

            Napi::Object o = Napi::Object::New(env);
            o.Set("data",        ab);
            o.Set("timestampNs", Napi::BigInt::New(env, p->timestamp_ns));
            o.Set("keyframe",    Napi::Boolean::New(env, p->keyframe));
            jsCb.Call({ o });
            delete p;
        });

    if (s == napi_ok) encoded_units_.fetch_add(1, std::memory_order_relaxed);
    else delete p;
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
