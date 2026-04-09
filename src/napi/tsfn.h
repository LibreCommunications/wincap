// ThreadSafeFunction helpers. The capture and audio threads must never
// block: all calls go through NonBlockingCall, and on napi_queue_full we
// drop the message (the consumer learns via getStats().droppedFrames).
#pragma once

#include <napi.h>

namespace wincap {

// Convenience: NonBlockingCall returning whether the message was queued.
template <typename DataPtr, typename Callback>
inline bool TsfnTryCall(Napi::ThreadSafeFunction& tsfn, DataPtr* data, Callback cb) {
    const napi_status s = tsfn.NonBlockingCall(data, cb);
    return s == napi_ok;
}

} // namespace wincap
