// HRESULT plumbing — converts Win32/WinRT failures into a typed exception
// the N-API layer translates to a JS Error with code + component tag.
#pragma once

#include <stdexcept>
#include <string>
#include <windows.h>

namespace wincap {

class HrError : public std::runtime_error {
public:
    HrError(HRESULT hr, const char* component, const char* what)
        : std::runtime_error(what), hr_(hr), component_(component) {}

    HRESULT hr() const noexcept { return hr_; }
    const char* component() const noexcept { return component_; }

    static std::string FormatHr(HRESULT hr);

private:
    HRESULT     hr_;
    const char* component_;
};

#define WINCAP_THROW_IF_FAILED(component, expr)                                     \
    do {                                                                            \
        const HRESULT _hr = (expr);                                                 \
        if (FAILED(_hr)) {                                                          \
            throw ::wincap::HrError(_hr, (component), #expr);                       \
        }                                                                           \
    } while (0)

} // namespace wincap
