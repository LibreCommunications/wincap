#include "core/common/errors.h"

#include <cstdio>

namespace wincap {

std::string HrError::FormatHr(HRESULT hr) {
    char buf[32];
    std::snprintf(buf, sizeof(buf), "0x%08lX", static_cast<unsigned long>(hr));
    return std::string(buf);
}

} // namespace wincap
