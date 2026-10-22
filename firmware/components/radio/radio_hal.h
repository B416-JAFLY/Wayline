/* ESP-IDF/M5Unified boundary for the Rust application. No protocol policy here. */
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

void owl_hal_init(void);
void owl_hal_radio_init(uint8_t mac[6]);
uint8_t owl_hal_default_channel(void);
int32_t owl_hal_apply(uint8_t channel, uint8_t filter);
int32_t owl_hal_set_channel(uint8_t channel);
int32_t owl_hal_set_mac(const uint8_t mac[6], uint8_t actual_mac[6], uint8_t channel, uint8_t filter);
int32_t owl_hal_transmit(uint8_t rate, const uint8_t *frame, size_t len);
void owl_hal_config_lock(void);
bool owl_hal_config_try_lock(void);
void owl_hal_config_unlock(void);
void owl_hal_state_lock(void);
void owl_hal_state_unlock(void);
void owl_hal_tx_lock(void);
void owl_hal_tx_unlock(void);
size_t owl_hal_read(uint8_t *out, size_t cap);
size_t owl_hal_write(const uint8_t *data, size_t len);
bool owl_hal_enqueue(uint8_t kind, const uint8_t *header, size_t header_len, const uint8_t *frame, size_t len);
const uint8_t *owl_hal_dequeue(size_t *len, bool wait);
void owl_hal_release(const uint8_t *item);
uint64_t owl_hal_time_us(void);
void owl_hal_delay_ms(uint32_t ms);
const char *owl_hal_target(void);
const char *owl_hal_idf_version(void);
void owl_hal_start_tasks(void);
uint32_t owl_hal_heap_free(void);
uint32_t owl_hal_heap_minimum(void);
uint32_t owl_hal_link_core(void);
bool owl_hal_ba_enabled(void);
void owl_hal_ba_init(void);
bool owl_hal_ba_enqueue(const uint8_t *frame, size_t len, uint32_t generation);
size_t owl_hal_ba_dequeue(uint8_t *frame, size_t cap, uint32_t *generation);
void owl_hal_ba_clear_queue(void);
bool owl_hal_ba_start(const uint8_t peer[6], uint8_t tid, uint16_t ssn, uint8_t window);
void owl_hal_ba_stop(void);

/* Rust callbacks. Metadata contains scalar values extracted from IDF bitfields. */
void owl_radio_main(void);
void owl_radio_command_task(void *arg);
void owl_radio_uplink_task(void *arg);
void owl_radio_aggregation_task(void *arg);
void owl_radio_ui_task(void *arg);
void owl_radio_rx(uint8_t kind, const uint8_t *frame, size_t len, uint32_t timestamp,
                  int8_t rssi, uint8_t channel, uint8_t rate, uint8_t sig_mode,
                  uint8_t mcs, uint8_t cwb, bool aggregated);
void owl_radio_tx_done(bool acked, const uint8_t destination[6]);
