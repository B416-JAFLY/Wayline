#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

uint16_t owl_proto_crc16(const uint8_t *data, size_t len);
size_t owl_proto_cobs_encode(const uint8_t *input, size_t input_len,
                             uint8_t *output, size_t output_cap);
ptrdiff_t owl_proto_cobs_decode_in_place(uint8_t *buffer, size_t len);

#ifdef __cplusplus
}
#endif
