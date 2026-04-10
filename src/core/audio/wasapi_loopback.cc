#include "core/audio/wasapi_loopback.h"

#include "core/common/clock.h"
#include "core/common/errors.h"
#include "core/common/mmcss.h"

#include <algorithm>
#include <cstring>
#include <utility>

#include <audioclientactivationparams.h>
#include <mmdeviceapi.h>
#include <functiondiscoverykeys_devpkey.h>
#include <combaseapi.h>

// IActivateAudioInterfaceCompletionHandler lives in mmdeviceapi.h on
// modern SDKs.

namespace wincap {

namespace {

// Completion handler for ActivateAudioInterfaceAsync. Signals an event
// when activation completes; the caller (Activate()) blocks on it.
class ActivateCompletionHandler
    : public Microsoft::WRL::RuntimeClass<
          Microsoft::WRL::RuntimeClassFlags<Microsoft::WRL::ClassicCom>,
          Microsoft::WRL::FtmBase,
          IActivateAudioInterfaceCompletionHandler> {
public:
    HANDLE done_event{nullptr};

    STDMETHOD(ActivateCompleted)(IActivateAudioInterfaceAsyncOperation*) override {
        if (done_event) ::SetEvent(done_event);
        return S_OK;
    }
};

} // namespace

WasapiLoopback::WasapiLoopback(WasapiLoopbackOptions opts) : opts_(opts) {
    pool_.resize(kPoolSize);
    for (auto& b : pool_) b.owner = this;
}

WasapiLoopback::~WasapiLoopback() { Stop(); }

void WasapiLoopback::ReleaseChunk(void* opaque) noexcept {
    auto* b = static_cast<PoolBuffer*>(opaque);
    b->in_use.store(0, std::memory_order_release);
}

WasapiLoopback::PoolBuffer* WasapiLoopback::AcquireBuffer(std::size_t needed_floats) noexcept {
    for (auto& b : pool_) {
        std::uint32_t expected = 0;
        if (b.in_use.compare_exchange_strong(expected, 1,
                std::memory_order_acq_rel, std::memory_order_relaxed)) {
            if (b.data.size() < needed_floats) b.data.resize(needed_floats);
            return &b;
        }
    }
    return nullptr;
}

void WasapiLoopback::Activate() {
    if (opts_.mode == LoopbackMode::SystemDefault) {
        Microsoft::WRL::ComPtr<IMMDeviceEnumerator> enumerator;
        WINCAP_THROW_IF_FAILED("wasapi_loopback",
            CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr,
                             CLSCTX_ALL, IID_PPV_ARGS(enumerator.GetAddressOf())));
        Microsoft::WRL::ComPtr<IMMDevice> device;
        WINCAP_THROW_IF_FAILED("wasapi_loopback",
            enumerator->GetDefaultAudioEndpoint(eRender, eConsole, device.GetAddressOf()));
        WINCAP_THROW_IF_FAILED("wasapi_loopback",
            device->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr,
                             reinterpret_cast<void**>(client_.GetAddressOf())));
        return;
    }

    // PROCESS_LOOPBACK (Win11 22000+).
    AUDIOCLIENT_ACTIVATION_PARAMS params{};
    params.ActivationType                              = AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK;
    params.ProcessLoopbackParams.TargetProcessId       = opts_.target_pid;
    params.ProcessLoopbackParams.ProcessLoopbackMode   =
        opts_.include_tree
            ? PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
            : PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE;

    PROPVARIANT prop_var{};
    prop_var.vt           = VT_BLOB;
    prop_var.blob.cbSize  = sizeof(params);
    prop_var.blob.pBlobData = reinterpret_cast<BYTE*>(&params);

    auto handler = Microsoft::WRL::Make<ActivateCompletionHandler>();
    handler->done_event = ::CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!handler->done_event) {
        throw HrError(HRESULT_FROM_WIN32(GetLastError()),
                      "wasapi_loopback", "CreateEvent failed");
    }

    Microsoft::WRL::ComPtr<IActivateAudioInterfaceAsyncOperation> async_op;
    WINCAP_THROW_IF_FAILED("wasapi_loopback",
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            __uuidof(IAudioClient),
            &prop_var,
            handler.Get(),
            async_op.GetAddressOf()));

    ::WaitForSingleObject(handler->done_event, INFINITE);
    ::CloseHandle(handler->done_event);

    HRESULT activate_hr = E_FAIL;
    Microsoft::WRL::ComPtr<IUnknown> punk;
    WINCAP_THROW_IF_FAILED("wasapi_loopback",
        async_op->GetActivateResult(&activate_hr, punk.GetAddressOf()));
    WINCAP_THROW_IF_FAILED("wasapi_loopback", activate_hr);
    WINCAP_THROW_IF_FAILED("wasapi_loopback",
        punk.As(&client_));
}

void WasapiLoopback::Initialize() {
    // Format: ask for the device mix format on system loopback. For
    // process loopback the API requires us to *supply* the format; we
    // pick 48 kHz float32 stereo which every modern endpoint supports.
    WAVEFORMATEX* mix_format = nullptr;
    WAVEFORMATEXTENSIBLE process_format{};

    if (opts_.mode == LoopbackMode::SystemDefault) {
        WINCAP_THROW_IF_FAILED("wasapi_loopback", client_->GetMixFormat(&mix_format));
    } else {
        process_format.Format.wFormatTag      = WAVE_FORMAT_EXTENSIBLE;
        process_format.Format.nChannels       = 2;
        process_format.Format.nSamplesPerSec  = 48000;
        process_format.Format.wBitsPerSample  = 32;
        process_format.Format.nBlockAlign     = 8;
        process_format.Format.nAvgBytesPerSec = 48000 * 8;
        process_format.Format.cbSize          = sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX);
        process_format.Samples.wValidBitsPerSample = 32;
        process_format.dwChannelMask          = SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT;
        process_format.SubFormat              = KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
        mix_format = reinterpret_cast<WAVEFORMATEX*>(&process_format);
    }

    DWORD flags = AUDCLNT_STREAMFLAGS_LOOPBACK |
                  AUDCLNT_STREAMFLAGS_EVENTCALLBACK |
                  AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM |
                  AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;

    HRESULT hr = E_FAIL;

    // Prefer IAudioClient3::InitializeSharedAudioStream which exposes the
    // engine's true minimum periodicity (often 3 ms on modern HW). Falls
    // back to a 20 ms buffer if the host doesn't support it (process
    // loopback path frequently doesn't).
    Microsoft::WRL::ComPtr<IAudioClient3> client3;
    if (opts_.mode == LoopbackMode::SystemDefault &&
        SUCCEEDED(client_.As(&client3))) {
        UINT32 default_period = 0, fundamental = 0, min_period = 0, max_period = 0;
        if (SUCCEEDED(client3->GetSharedModeEnginePeriod(
                mix_format, &default_period, &fundamental, &min_period, &max_period))) {
            const UINT32 period = std::max<UINT32>(min_period, fundamental);
            hr = client3->InitializeSharedAudioStream(
                flags, period, mix_format, nullptr);
        }
    }

    if (FAILED(hr)) {
        // 20 ms buffer (in 100-ns units) fallback.
        constexpr REFERENCE_TIME kBufferDurationHns = 20 * 10000;
        hr = client_->Initialize(
            AUDCLNT_SHAREMODE_SHARED, flags, kBufferDurationHns, 0, mix_format, nullptr);
    }
    if (FAILED(hr)) {
        if (mix_format && opts_.mode == LoopbackMode::SystemDefault) CoTaskMemFree(mix_format);
        WINCAP_THROW_IF_FAILED("wasapi_loopback", hr);
    }

    format_ = AudioFormat::FromWaveFormat(mix_format);
    if (mix_format && opts_.mode == LoopbackMode::SystemDefault) CoTaskMemFree(mix_format);

    event_ = ::CreateEventW(nullptr, FALSE, FALSE, nullptr);
    WINCAP_THROW_IF_FAILED("wasapi_loopback", client_->SetEventHandle(event_));
    WINCAP_THROW_IF_FAILED("wasapi_loopback",
        client_->GetService(IID_PPV_ARGS(capture_.GetAddressOf())));
}

void WasapiLoopback::Start(AudioCallback cb, AudioErrorCallback err) {
    if (running_.exchange(true)) return;
    cb_     = std::move(cb);
    err_cb_ = std::move(err);

    stop_event_ = ::CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!stop_event_) {
        running_.store(false);
        throw HrError(HRESULT_FROM_WIN32(GetLastError()),
                      "wasapi_loopback", "CreateEvent failed");
    }

    thread_ = std::thread(&WasapiLoopback::ThreadMain, this);
}

void WasapiLoopback::Stop() {
    if (!running_.exchange(false)) return;
    if (stop_event_) ::SetEvent(stop_event_);
    if (thread_.joinable()) thread_.join();
    if (stop_event_) { ::CloseHandle(stop_event_); stop_event_ = nullptr; }
    ReleaseClient();
    cb_     = nullptr;
    err_cb_ = nullptr;
}

void WasapiLoopback::ReleaseClient() noexcept {
    if (client_) { client_->Stop(); }
    capture_.Reset();
    client_.Reset();
    if (event_) { ::CloseHandle(event_); event_ = nullptr; }
}

void WasapiLoopback::ThreadMain() {
    HRESULT co = ::CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    MmcssScope mmcss(L"Pro Audio");

    try {
        Activate();
        Initialize();
        WINCAP_THROW_IF_FAILED("wasapi_loopback", client_->Start());
    } catch (HrError const& e) {
        if (err_cb_) err_cb_(e.component(), e.hr(), e.what());
        if (SUCCEEDED(co)) ::CoUninitialize();
        return;
    }

    HANDLE waits[2] = { event_, stop_event_ };
    while (running_.load(std::memory_order_acquire)) {
        DWORD r = ::WaitForMultipleObjects(2, waits, FALSE, 1000);
        if (r == WAIT_OBJECT_0 + 1) break; // stop
        if (r == WAIT_TIMEOUT) continue;
        if (r != WAIT_OBJECT_0) break;

        UINT32 packet = 0;
        while (SUCCEEDED(capture_->GetNextPacketSize(&packet)) && packet > 0) {
            BYTE*   data = nullptr;
            UINT32  frames = 0;
            DWORD   flags = 0;
            UINT64  device_pos = 0;
            UINT64  qpc_pos = 0;

            HRESULT hr = capture_->GetBuffer(&data, &frames, &flags, &device_pos, &qpc_pos);
            if (hr == AUDCLNT_S_BUFFER_EMPTY) break;
            if (FAILED(hr)) {
                if (err_cb_) err_cb_("wasapi_loopback", hr, "GetBuffer failed");
                break;
            }

            const std::size_t needed_floats =
                static_cast<std::size_t>(frames) * format_.channels;
            PoolBuffer* slot = AcquireBuffer(needed_floats);
            if (!slot) {
                // Pool exhausted: drop this packet (consumer too slow).
                capture_->ReleaseBuffer(frames);
                continue;
            }

            const bool silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT) != 0;
            if (silent) {
                std::memset(slot->data.data(), 0, needed_floats * sizeof(float));
            } else {
                std::memcpy(slot->data.data(), data, needed_floats * sizeof(float));
            }

            AudioChunk chunk{};
            chunk.data           = slot->data.data();
            chunk.frame_count    = frames;
            chunk.channels       = format_.channels;
            chunk.sample_rate    = format_.sample_rate;
            // qpc_pos from WASAPI is in QPC ticks already.
            chunk.timestamp_ns   = Clock::TicksToNs(qpc_pos);
            chunk.silent         = silent;
            chunk.discontinuity  = (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY) != 0;
            chunk.release_fn     = &WasapiLoopback::ReleaseChunk;
            chunk.release_opaque = slot;

            if (cb_) cb_(chunk);
            else ReleaseChunk(slot);

            capture_->ReleaseBuffer(frames);
        }
    }

    ReleaseClient();
    if (SUCCEEDED(co)) ::CoUninitialize();
}

} // namespace wincap
