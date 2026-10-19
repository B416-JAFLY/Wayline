#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct {
	uint32_t rx;
	uint32_t rx_drop;
	uint32_t tx_ok;
	uint32_t tx_err;
	uint32_t tx_acked;
	uint32_t tx_noack;
	uint32_t bad_msg;
} radio_ui_stats_t;

typedef struct {
	uint8_t mac[6];
	int8_t rssi;
	uint16_t psf;
	uint16_t mif;
	uint32_t last_seen_ms;
} radio_ui_peer_t;

#ifdef __cplusplus
extern "C" {
#endif

void radio_ui_start(void);
/* Atomic snapshot for the optional STATS/UI diagnostic request. */
uint32_t radio_ui_debug_state(void);
bool radio_ui_poll_key(char *key, bool *pressed);
void radio_ui_set_brightness(uint8_t brightness);
void radio_ui_set_volume(uint8_t volume);
/* Samples must remain alive until asynchronous playback finishes. */
bool radio_ui_tone(uint16_t frequency, uint16_t duration_ms, const uint8_t *samples, size_t len);
void radio_ui_stop_sound(void);
/* Rust renders RGB565 frames; these functions only present and persist preferences. */
void radio_ui_present(const uint16_t *pixels);
uint32_t radio_ui_load_preferences(void);
bool radio_ui_save_preferences(uint32_t preferences);

#ifdef __cplusplus
}
#endif
