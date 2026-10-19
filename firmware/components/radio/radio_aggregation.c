/* Experimental receive BA offload for the exact ESP32-S3 libpp ABI checked by
 * CMake. No software control-frame injection and no changes to SDK binaries.
 * All table access runs under radio_hal.c's configuration mutex, outside the
 * Rust state spinlock. This radio never associates or starts a SoftAP, so BA
 * table slot 0 is exclusively ours. Connecting this radio to an AP would
 * invalidate that ownership contract and is explicitly rejected below. */
#include "radio_hal.h"
#include "sdkconfig.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include <string.h>

#if CONFIG_OWL_RX_AGGREGATION
#include "esp_idf_version.h"
#include "soc/soc.h"
#if !CONFIG_IDF_TARGET_ESP32S3 || ESP_IDF_VERSION != ESP_IDF_VERSION_VAL(6, 0, 0)
#error "Receive BA offload requires the pinned ESP32-S3 ESP-IDF 6.0 ABI"
#endif

/* Verified against libpp.a's hal_ampdu.o (see CMake SHA-256 gate):
 * a2=index:u8, a3=ifx:u8, a4=peer:6 readable bytes, a5=tid:u8,
 * a6=starting sequence:u16 (12 bits), a7=window:u8. The peer is copied
 * synchronously into registers; its pointer is never retained. */
extern void hal_agreement_add_rx_ba(uint8_t index, uint8_t ifx, const uint8_t peer[6],
                                   uint8_t tid, uint16_t ssn, uint8_t window);
extern void hal_agreement_del_rx_ba(uint8_t index);

enum { BA_CONTROL_MAX = 64, BA_CONTROL_DEPTH = 8 };
struct control_frame { uint32_t generation; uint8_t len; uint8_t bytes[BA_CONTROL_MAX]; };
static QueueHandle_t control_queue;

/* Slot-0 layout verified in the same pinned hal_ampdu.o. Readback checks that
 * the requested peer/TID/window was installed; it does not prove over-air BA. */
#define BA_CONTROL_REG 0x60033274u
#define BA_PEER_HI_REG 0x60033278u
#define BA_PEER_LO_REG 0x6003327cu
#define BA_WINDOW_REG  0x60033280u
#endif

bool owl_hal_ba_enabled(void)
{
#if CONFIG_OWL_RX_AGGREGATION
	return true;
#else
	return false;
#endif
}

void owl_hal_ba_init(void)
{
#if CONFIG_OWL_RX_AGGREGATION
	control_queue = xQueueCreate(BA_CONTROL_DEPTH, sizeof(struct control_frame));
	ESP_ERROR_CHECK(control_queue ? ESP_OK : ESP_ERR_NO_MEM);
#endif
}

bool owl_hal_ba_enqueue(const uint8_t *frame, size_t len, uint32_t generation)
{
#if CONFIG_OWL_RX_AGGREGATION
	if (!frame || len > BA_CONTROL_MAX || !control_queue) return false;
	struct control_frame item = { .generation = generation, .len = (uint8_t)len };
	memcpy(item.bytes, frame, len);
	return xQueueSend(control_queue, &item, 0) == pdTRUE;
#else
	(void)frame; (void)len; (void)generation;
	return false;
#endif
}

size_t owl_hal_ba_dequeue(uint8_t *frame, size_t cap, uint32_t *generation)
{
#if CONFIG_OWL_RX_AGGREGATION
	struct control_frame item;
	if (!frame || !generation || cap < BA_CONTROL_MAX ||
	    xQueueReceive(control_queue, &item, pdMS_TO_TICKS(10)) != pdTRUE) return 0;
	memcpy(frame, item.bytes, item.len);
	*generation = item.generation;
	return item.len;
#else
	(void)frame; (void)cap; (void)generation;
	return 0;
#endif
}

void owl_hal_ba_clear_queue(void)
{
#if CONFIG_OWL_RX_AGGREGATION
	if (control_queue) xQueueReset(control_queue);
#endif
}

bool owl_hal_ba_start(const uint8_t peer[6], uint8_t tid, uint16_t ssn, uint8_t window)
{
#if CONFIG_OWL_RX_AGGREGATION
	wifi_mode_t mode;
	wifi_ap_record_t ap;
	if (!peer || (peer[0] & 1) || tid >= 8 || ssn >= 4096 || window < 2 || window > 16 ||
	    esp_wifi_get_mode(&mode) != ESP_OK || mode != WIFI_MODE_STA ||
	    esp_wifi_sta_get_ap_info(&ap) != ESP_ERR_WIFI_NOT_CONNECT) return false;
	hal_agreement_del_rx_ba(0);
	hal_agreement_add_rx_ba(0, WIFI_IF_STA, peer, tid, ssn, window);
	uint32_t lo = (uint32_t)peer[0] | ((uint32_t)peer[1] << 8) |
	              ((uint32_t)peer[2] << 16) | ((uint32_t)peer[3] << 24);
	uint32_t hi = (uint32_t)peer[4] | ((uint32_t)peer[5] << 8);
	if ((REG_READ(BA_CONTROL_REG) & 0xc000f007u) == (0xc0000005u | ((uint32_t)tid << 12)) &&
	    REG_READ(BA_PEER_LO_REG) == lo && (REG_READ(BA_PEER_HI_REG) & 0x3ffffu) == hi &&
	    ((REG_READ(BA_WINDOW_REG) >> 13) & 0x7fu) == window) return true;
	hal_agreement_del_rx_ba(0);
#else
	(void)peer; (void)tid; (void)ssn; (void)window;
#endif
	return false;
}

void owl_hal_ba_stop(void)
{
#if CONFIG_OWL_RX_AGGREGATION
	hal_agreement_del_rx_ba(0);
#endif
}
