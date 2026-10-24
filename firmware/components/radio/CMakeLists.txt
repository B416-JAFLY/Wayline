set(OWL_RADIO_SRCS "radio_hal.c" "radio_aggregation.c")
set(OWL_RADIO_REQUIRES esp_wifi esp_timer nvs_flash esp_ringbuf esp_driver_uart
                       esp_driver_usb_serial_jtag)

if(CONFIG_OWL_CARDPUTER_UI)
    list(APPEND OWL_RADIO_SRCS "radio_ui.cpp")
    list(APPEND OWL_RADIO_REQUIRES m5unified esp_driver_i2c)
endif()

# Rust references the UI object after IDF's component archives on the link line.
# Retain the bridge component so its callbacks participate in resolution.
idf_component_register(SRCS ${OWL_RADIO_SRCS}
                       INCLUDE_DIRS "."
                       PRIV_INCLUDE_DIRS "../../../crates/wayline-proto/include"
                       PRIV_REQUIRES ${OWL_RADIO_REQUIRES}
                       WHOLE_ARCHIVE)

if(NOT CONFIG_IDF_TARGET_ESP32S3 AND NOT CONFIG_IDF_TARGET_ESP32C5)
    message(FATAL_ERROR "Rust radio firmware supports ESP32-S3 or ESP32-C5")
endif()
if(CONFIG_OWL_RX_AGGREGATION)
    if(NOT CONFIG_IDF_TARGET_ESP32S3)
        message(FATAL_ERROR "Experimental RX aggregation is supported only on ESP32-S3")
    endif()
    # Internal hardware BA ABI is not a public ESP-IDF contract. Refuse unknown
    # archives, even if they use the same version string; never patch SDK files.
    file(SHA256 "$ENV{IDF_PATH}/components/esp_wifi/lib/esp32s3/libpp.a" OWL_LIBPP_SHA256)
    if(NOT OWL_LIBPP_SHA256 STREQUAL "47da9a1daf730f5d4867884c9ef6bf5e57ff67265fe1d18c9ccc9d0f3a22b6fd")
        message(FATAL_ERROR "Experimental RX aggregation requires the audited ESP32-S3 ESP-IDF 6.0 libpp.a")
    endif()
endif()
if(CONFIG_IDF_TARGET_ESP32C5)
    set(OWL_RUST_TARGET "riscv32imac-esp-espidf" CACHE STRING
        "Rust target triple used by the radio application")
    set(OWL_RUST_TOOLCHAIN "esp" CACHE STRING
        "Rust toolchain used by the radio application")
    set(OWL_RUST_BUILD_FLAGS -Z build-std=core)
else()
    set(OWL_RUST_TARGET "xtensa-esp32s3-espidf" CACHE STRING
        "Rust target triple used by the radio application")
    set(OWL_RUST_TOOLCHAIN "esp" CACHE STRING
        "Rust toolchain used by the radio application")
    set(OWL_RUST_BUILD_FLAGS -Z build-std=core)
endif()
set(OWL_RUST_DIR "${CMAKE_CURRENT_LIST_DIR}/../..")
set(OWL_RUST_TARGET_DIR "${CMAKE_BINARY_DIR}/rust-target")
set(OWL_RUST_LIB "${OWL_RUST_TARGET_DIR}/${OWL_RUST_TARGET}/release/libowl_radio_rust_link.a")
set(OWL_RUST_FEATURES)
if(CONFIG_OWL_CARDPUTER_UI)
    list(APPEND OWL_RUST_FEATURES --features cardputer-ui)
endif()

add_custom_target(owl_rust_link ALL
    COMMAND ${CMAKE_COMMAND} -E env "CARGO_TARGET_DIR=${OWL_RUST_TARGET_DIR}"
            cargo +${OWL_RUST_TOOLCHAIN} build --manifest-path "${OWL_RUST_DIR}/Cargo.toml"
            --target ${OWL_RUST_TARGET} --release --locked ${OWL_RUST_BUILD_FLAGS}
            ${OWL_RUST_FEATURES}
    BYPRODUCTS "${OWL_RUST_LIB}"
    WORKING_DIRECTORY "${OWL_RUST_DIR}"
    COMMENT "Compiling Rust radio application (ESP-IDF drivers)")
add_dependencies(${COMPONENT_LIB} owl_rust_link)
target_include_directories(${COMPONENT_LIB} PRIVATE "${OWL_RUST_DIR}/include")
target_link_libraries(${COMPONENT_LIB} INTERFACE "${OWL_RUST_LIB}")
