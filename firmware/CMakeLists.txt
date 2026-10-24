# owl-radio: Rust firmware with ESP-IDF drivers, providing an ESP32 AWDL radio
# for OWL on Windows, which has no Wi-Fi
# driver that can inject frames. Promiscuous receive and raw transmit over a
# serial link (../crates/wayline-proto/include/proto.h).
#   idf.py set-target esp32s3   && idf.py build flash
#   idf.py set-target esp32c5   && idf.py build flash
cmake_minimum_required(VERSION 3.16)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(owl-radio)
