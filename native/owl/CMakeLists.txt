# owl.exe: OWL for Windows, with an ESP32 running owl-radio (firmware/) as its
# radio and a Wintun adapter as awdl0. Upstream's protocol code (src/), core.c
# and owl.c build unchanged; io.c, netutils.c and libev are replaced by the
# files here, and compat/ supplies the POSIX headers they include.
#
#   scripts/build-owl.ps1 fetches OWL at the pinned commit with native/owl/patches/ and
#   Wintun, then builds this with MinGW (GCC).
cmake_minimum_required(VERSION 3.16)
project(owl-windows C)

get_filename_component(OMDROP_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." ABSOLUTE)
set(OWL_DIR "${OMDROP_ROOT}/.cache/upstream/owl" CACHE PATH "OWL source, patched")
set(WINTUN_DIR "${OMDROP_ROOT}/.cache/upstream/wintun" CACHE PATH "Wintun SDK (include/, bin/)")
set(PROTO_DIR "${OMDROP_ROOT}/crates/omdrop-proto/include")

if (NOT EXISTS "${OWL_DIR}/src/state.c")
    message(FATAL_ERROR "no OWL source at ${OWL_DIR}; run scripts/build-owl.ps1")
endif ()

file(GLOB OWL_SRC "${OWL_DIR}/src/*.c")

add_executable(owl
        ${OWL_SRC}
        "${OWL_DIR}/radiotap/radiotap.c"
        "${OWL_DIR}/daemon/core.c"
        "${OWL_DIR}/daemon/owl.c"
        io_esp.c
        netutils_win.c
        ev_win.c
        compat.c)

function(configure_owl_target target)
    target_include_directories(${target} PRIVATE
        compat
        "${CMAKE_CURRENT_SOURCE_DIR}"
        "${OWL_DIR}/src"
        "${OWL_DIR}/radiotap"
        "${WINTUN_DIR}/include"
        "${PROTO_DIR}")

    target_compile_definitions(${target} PRIVATE _DEFAULT_SOURCE RADIOTAP_SUPPORT_OVERRIDES _WIN32_WINNT=0x0A00)
    target_compile_options(${target} PRIVATE
        -include "${CMAKE_CURRENT_SOURCE_DIR}/compat/owl_win.h"
        # daemon/io.h would shadow the CRT's <io.h>, which <unistd.h> includes.
        "-iquote${OWL_DIR}/daemon"
        # GCC's packed-struct layout, which OWL's frame structs are written for;
        # MinGW otherwise lays bitfields out as MSVC does.
        -mno-ms-bitfields
        -Wall -Wextra -Wno-unused-parameter -Wno-format
        # radiotap.c casts pointers to unsigned long for alignment arithmetic,
        # which is 32 bits on Windows; the low bits are all it needs.
        -Wno-pointer-to-int-cast -Wno-address-of-packed-member -Wno-unknown-pragmas)
    target_link_libraries(${target} PRIVATE ws2_32 iphlpapi winmm advapi32 -static)
endfunction()

configure_owl_target(owl)

include(CTest)
if(BUILD_TESTING)
    # Test the same patched protocol sources and Windows ABI as owl.exe. These
    # executables do not open a serial port or create a Wintun adapter.
    add_library(owl_test_protocol STATIC ${OWL_SRC}
        "${OWL_DIR}/radiotap/radiotap.c" compat.c)
    configure_owl_target(owl_test_protocol)
    include(CheckCCompilerFlag)
    check_c_compiler_flag("-ftrivial-auto-var-init=pattern" OWL_HAS_PATTERN_INIT)
    if(OWL_HAS_PATTERN_INIT)
        # Make missing initialization deterministic, including radiotap Flags.
        target_compile_options(owl_test_protocol PRIVATE -ftrivial-auto-var-init=pattern)
    endif()

    foreach(test_name IN ITEMS amsdu padding rx_flags peers sync_channels)
        add_executable(owl_test_${test_name} tests/owl_${test_name}.c)
        configure_owl_target(owl_test_${test_name})
        target_link_libraries(owl_test_${test_name} PRIVATE owl_test_protocol)
        add_test(NAME owl_${test_name} COMMAND owl_test_${test_name})
    endforeach()

    add_executable(owl_test_tx_backpressure tests/owl_tx_backpressure.c
        "${OWL_DIR}/daemon/core.c" netutils_win.c ev_win.c)
    configure_owl_target(owl_test_tx_backpressure)
    target_link_libraries(owl_test_tx_backpressure PRIVATE owl_test_protocol)
    add_test(NAME owl_tx_backpressure COMMAND owl_test_tx_backpressure)
    add_executable(owl_test_transfer_ui tests/owl_transfer_ui.c netutils_win.c ev_win.c)
    configure_owl_target(owl_test_transfer_ui)
    target_link_libraries(owl_test_transfer_ui PRIVATE owl_test_protocol)
    add_test(NAME owl_transfer_ui COMMAND owl_test_transfer_ui)
    add_executable(owl_test_radio_io tests/owl_radio_io.c netutils_win.c ev_win.c)
    configure_owl_target(owl_test_radio_io)
    target_link_libraries(owl_test_radio_io PRIVATE owl_test_protocol)
    add_test(NAME owl_radio_io COMMAND owl_test_radio_io)
    set_tests_properties(owl_amsdu owl_padding owl_rx_flags owl_peers owl_tx_backpressure
        owl_transfer_ui owl_sync_channels owl_radio_io
        PROPERTIES TIMEOUT 10)
endif()

# wintun.dll goes beside owl.exe.
if (CMAKE_SYSTEM_PROCESSOR MATCHES "ARM64|aarch64")
    set(WINTUN_ARCH arm64)
else ()
    set(WINTUN_ARCH amd64)
endif ()
add_custom_command(TARGET owl POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different "${WINTUN_DIR}/bin/${WINTUN_ARCH}/wintun.dll" "$<TARGET_FILE_DIR:owl>")
