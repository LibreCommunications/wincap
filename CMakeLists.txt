cmake_minimum_required(VERSION 3.25)
project(wincap CXX)

if(NOT WIN32)
    message(FATAL_ERROR "wincap targets Windows only.")
endif()

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# ---------------------------------------------------------------------------
# cmake-js integration: CMAKE_JS_INC, CMAKE_JS_SRC, CMAKE_JS_LIB are provided
# by cmake-js. When building standalone (e.g. for native unit tests) we let
# users opt in via -DWINCAP_BUILD_TESTS=ON.
# ---------------------------------------------------------------------------
option(WINCAP_BUILD_TESTS "Build native GoogleTest unit tests" OFF)
option(WINCAP_ENABLE_WERROR "Treat warnings as errors" ON)

# Pull node-addon-api include path. cmake-js doesn't set this automatically.
execute_process(
    COMMAND node -p "require('node-addon-api').include_dir"
    WORKING_DIRECTORY ${CMAKE_SOURCE_DIR}
    OUTPUT_VARIABLE NODE_ADDON_API_DIR
    OUTPUT_STRIP_TRAILING_WHITESPACE
    ERROR_QUIET
)

add_definitions(
    -DNAPI_VERSION=8
    -DNAPI_CPP_EXCEPTIONS
    -DNOMINMAX
    -DWIN32_LEAN_AND_MEAN
    -DUNICODE
    -D_UNICODE
    -DWINRT_LEAN_AND_MEAN
    -D_WIN32_WINNT=0x0A00  # Windows 10+
)

set(WINCAP_CORE_SOURCES
    src/core/common/clock.cc
    src/core/common/frame_pool.cc
    src/core/common/mmcss.cc
    src/core/common/errors.cc
    src/core/gfx/d3d_device.cc
    src/core/gfx/video_processor.cc
    src/core/capture/wgc_source.cc
    src/core/audio/format.cc
    src/core/audio/wasapi_loopback.cc
    src/core/encoder/mf_encoder.cc
)

set(WINCAP_NAPI_SOURCES
    src/napi/addon.cc
    src/napi/capture_session.cc
    src/napi/audio_session.cc
    src/napi/sources.cc
    src/napi/tsfn.cc
)

add_library(${PROJECT_NAME} SHARED
    ${WINCAP_CORE_SOURCES}
    ${WINCAP_NAPI_SOURCES}
    ${CMAKE_JS_SRC}
)

set_target_properties(${PROJECT_NAME} PROPERTIES
    PREFIX ""
    SUFFIX ".node"
    CXX_VISIBILITY_PRESET hidden
    POSITION_INDEPENDENT_CODE ON
)

target_include_directories(${PROJECT_NAME} PRIVATE
    ${CMAKE_SOURCE_DIR}/src
    ${CMAKE_JS_INC}
    ${NODE_ADDON_API_DIR}
)

target_link_libraries(${PROJECT_NAME} PRIVATE
    ${CMAKE_JS_LIB}
    d3d11
    dxgi
    dxguid
    dwmapi
    windowsapp        # WinRT (Windows.Graphics.Capture)
    runtimeobject     # RoGetActivationFactory etc.
    avrt              # MMCSS
    mmdevapi
    mfplat            # Media Foundation
    mf
    mfuuid
    wmcodecdspuuid
    ksuser            # KSDATAFORMAT_SUBTYPE_*
    ole32
    oleaut32
    uuid
    user32
    gdi32
    shcore
)

if(MSVC)
    target_compile_options(${PROJECT_NAME} PRIVATE
        /W4 /permissive- /Zc:__cplusplus /utf-8 /EHsc /await
        /Gy /Gw
        # node-addon-api's napi-inl.h trips C4127 (conditional expression
        # is constant) under /W4. It's third-party; disable globally
        # rather than peppering pragma push/pops at every include site.
        /wd4127
        $<$<CONFIG:Release>:/O2 /Oi /GL>
    )
    if(WINCAP_ENABLE_WERROR)
        target_compile_options(${PROJECT_NAME} PRIVATE /WX)
    endif()
    target_link_options(${PROJECT_NAME} PRIVATE
        $<$<CONFIG:Release>:/LTCG /OPT:REF /OPT:ICF>
    )
    # Embed manifest for Per-Monitor V2 DPI awareness.
    target_sources(${PROJECT_NAME} PRIVATE src/napi/wincap.manifest)
endif()

# Generate node.lib delay-load stub on cmake-js builds where needed.
if(MSVC AND CMAKE_JS_NODELIB_DEF AND CMAKE_JS_NODELIB_TARGET)
    execute_process(COMMAND ${CMAKE_AR} /def:${CMAKE_JS_NODELIB_DEF} /out:${CMAKE_JS_NODELIB_TARGET} ${CMAKE_STATIC_LINKER_FLAGS})
endif()

if(WINCAP_BUILD_TESTS)
    enable_testing()
    add_subdirectory(test/unit)
endif()
