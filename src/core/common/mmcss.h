// RAII wrapper for MMCSS thread characteristics (AvSetMmThreadCharacteristics).
// Use "Capture" for the WGC pump thread and "Pro Audio" for the WASAPI loop.
#pragma once

#include <windows.h>

namespace wincap {

class MmcssScope {
public:
    explicit MmcssScope(const wchar_t* task_name) noexcept;
    ~MmcssScope();

    MmcssScope(const MmcssScope&) = delete;
    MmcssScope& operator=(const MmcssScope&) = delete;

    bool Ok() const noexcept { return handle_ != nullptr; }

private:
    HANDLE handle_{nullptr};
    DWORD  task_index_{0};
};

} // namespace wincap
