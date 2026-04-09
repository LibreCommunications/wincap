#include "core/common/mmcss.h"

#include <avrt.h>

namespace wincap {

MmcssScope::MmcssScope(const wchar_t* task_name) noexcept {
    handle_ = AvSetMmThreadCharacteristicsW(task_name, &task_index_);
    if (handle_) {
        AvSetMmThreadPriority(handle_, AVRT_PRIORITY_HIGH);
    }
}

MmcssScope::~MmcssScope() {
    if (handle_) {
        AvRevertMmThreadCharacteristics(handle_);
        handle_ = nullptr;
    }
}

} // namespace wincap
