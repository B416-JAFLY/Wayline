/* Driver glue only: the Rust static library owns the application and protocol. */
#include "radio_hal.h"
#include "proto.h"
#include "esp_event.h"
#include "esp_idf_version.h"
#include "esp_heap_caps.h"
#include "esp_mac.h"
#include "esp_timer.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/ringbuf.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "nvs_flash.h"
#include <string.h>
#if CONFIG_OWL_RADIO_LINK_USB_SERIAL_JTAG
#include "driver/usb_serial_jtag.h"
#else
#include "driver/uart.h"
#include "soc/uart_pins.h"
#endif
#if CONFIG_OWL_CARDPUTER_UI
#include "radio_ui.h"
_Static_assert(sizeof(radio_ui_stats_t) == 28, "Rust Stats ABI");
_Static_assert(sizeof(radio_ui_peer_t) == 16, "Rust Peer ABI");
_Static_assert(offsetof(radio_ui_peer_t, psf) == 8, "Rust Peer.psf ABI");
_Static_assert(offsetof(radio_ui_peer_t, last_seen_ms) == 12, "Rust Peer.last_seen_ms ABI");
#endif

static RingbufHandle_t rx_ring;
static SemaphoreHandle_t tx_lock, config_lock;
static portMUX_TYPE state_lock = portMUX_INITIALIZER_UNLOCKED;
#if !CONFIG_IDF_TARGET_ESP32C5
static int current_rate = -1;
#else
#define OWL_C5_DEFAULT_CHANNEL 6
static uint8_t current_channel = OWL_C5_DEFAULT_CHANNEL;
static bool c5_promiscuous_enabled;

static esp_err_t c5_set_promiscuous(bool enabled)
{
	esp_err_t err = esp_wifi_set_promiscuous(enabled);
	if (err == ESP_OK) c5_promiscuous_enabled = enabled;
	return err;
}
#endif
static uint32_t link_core;

#if CONFIG_OWL_RADIO_LINK_USB_SERIAL_JTAG && !CONFIG_FREERTOS_UNICORE
/* The IDF driver attaches its interrupt to the installing CPU. Keep its 64-byte
 * FIFO service away from the Wi-Fi task/interrupt load. If driver uninstall is
 * ever added, it must also run on this core. No driver or SDK patch is needed. */
#if CONFIG_ESP_WIFI_TASK_PINNED_TO_CORE_1
#define OWL_LINK_CORE 0
#else
#define OWL_LINK_CORE 1
#endif
struct link_init { TaskHandle_t caller; volatile esp_err_t result; };
static void link_init_task(void *arg)
{
	struct link_init *init = arg;
	TaskHandle_t caller = init->caller;
	usb_serial_jtag_driver_config_t cfg = { .tx_buffer_size = 8192, .rx_buffer_size = 8192 };
	link_core = xPortGetCoreID();
	init->result = usb_serial_jtag_driver_install(&cfg);
	/* Do not touch init after waking its owner: it lives on the caller's stack. */
	xTaskNotifyGive(caller);
	vTaskDelete(NULL);
}
#else
#define OWL_LINK_CORE tskNO_AFFINITY
#endif

void owl_hal_state_lock(void) { portENTER_CRITICAL(&state_lock); }
void owl_hal_state_unlock(void) { portEXIT_CRITICAL(&state_lock); }
void owl_hal_tx_lock(void) { xSemaphoreTake(tx_lock, portMAX_DELAY); }
void owl_hal_tx_unlock(void) { xSemaphoreGive(tx_lock); }
void owl_hal_config_lock(void) { xSemaphoreTake(config_lock, portMAX_DELAY); }
bool owl_hal_config_try_lock(void) { return xSemaphoreTake(config_lock, 0) == pdTRUE; }
void owl_hal_config_unlock(void) { xSemaphoreGive(config_lock); }

#if CONFIG_IDF_TARGET_ESP32C5
static esp_err_t configure_c5_tx_rate(void)
{
	/* ESP-IDF 6.0 requires this fixed C5 PHY/rate configuration before start.
	 * HE20/MCS0 works on both bands; host per-frame rates are ignored. */
	wifi_tx_rate_config_t config = {
		.phymode = WIFI_PHY_MODE_HE20,
		.rate = WIFI_PHY_RATE_MCS0_LGI,
		.ersu = false,
		.dcm = false,
	};
	return esp_wifi_config_80211_tx(WIFI_IF_STA, &config);
}
#endif

static void promisc_cb(void *buffer, wifi_promiscuous_pkt_type_t type)
{
	const wifi_promiscuous_pkt_t *packet = buffer;
	const wifi_pkt_rx_ctrl_t *rc = &packet->rx_ctrl;
	if (type == WIFI_PKT_MISC || rc->sig_len < 14 || rc->rx_state != 0)
		return;
	size_t len = rc->sig_len - 4;
	if (len > PROTO_MAX_FRAME)
		return;
	uint8_t kind = type == WIFI_PKT_MGMT ? 0 : type == WIFI_PKT_DATA ? 1 : 2;
#if !CONFIG_SOC_WIFI_HE_SUPPORT
	owl_radio_rx(kind, packet->payload, len, rc->timestamp, rc->rssi, rc->channel,
	             rc->rate, rc->sig_mode, rc->mcs, rc->cwb, rc->aggregation != 0);
#else
	owl_radio_rx(kind, packet->payload, len, rc->timestamp, rc->rssi, rc->channel, 0, 0, 0, 0, false);
#endif
}

static void tx_done_cb(const esp_80211_tx_info_t *info)
{
	owl_radio_tx_done(info->tx_status == WIFI_SEND_SUCCESS, info->des_addr);
}

void owl_hal_init(void)
{
	tx_lock = xSemaphoreCreateMutex();
	config_lock = xSemaphoreCreateMutex();
	rx_ring = xRingbufferCreate(48 * 1024, RINGBUF_TYPE_NOSPLIT);
	ESP_ERROR_CHECK(tx_lock && config_lock && rx_ring ? ESP_OK : ESP_ERR_NO_MEM);
	owl_hal_ba_init();
#if CONFIG_OWL_RADIO_LINK_USB_SERIAL_JTAG
#if !CONFIG_FREERTOS_UNICORE
	/* app_main owns its unused notification slot until owl_hal_init returns. */
	struct link_init init = { .caller = xTaskGetCurrentTaskHandle(), .result = ESP_FAIL };
	ESP_ERROR_CHECK(xTaskCreatePinnedToCore(link_init_task, "usb_init", 3072, &init, 12,
	                                      NULL, OWL_LINK_CORE) == pdPASS ? ESP_OK : ESP_ERR_NO_MEM);
	ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
	ESP_ERROR_CHECK(init.result);
#else
	usb_serial_jtag_driver_config_t cfg = { .tx_buffer_size = 8192, .rx_buffer_size = 8192 };
	link_core = xPortGetCoreID();
	ESP_ERROR_CHECK(usb_serial_jtag_driver_install(&cfg));
#endif
#else
	link_core = xPortGetCoreID();
	const uart_config_t cfg = {
		.baud_rate = CONFIG_OWL_RADIO_BAUD, .data_bits = UART_DATA_8_BITS,
		.parity = UART_PARITY_DISABLE, .stop_bits = UART_STOP_BITS_1,
		.flow_ctrl = UART_HW_FLOWCTRL_DISABLE, .source_clk = UART_SCLK_DEFAULT,
	};
	ESP_ERROR_CHECK(uart_driver_install(UART_NUM_0, 8192, 16384, 0, NULL, 0));
	ESP_ERROR_CHECK(uart_param_config(UART_NUM_0, &cfg));
	ESP_ERROR_CHECK(uart_set_pin(UART_NUM_0, U0TXD_GPIO_NUM, U0RXD_GPIO_NUM,
	                             UART_PIN_NO_CHANGE, UART_PIN_NO_CHANGE));
#endif
}

void owl_hal_radio_init(uint8_t mac[6])
{
	esp_err_t err = nvs_flash_init();
	if (err == ESP_ERR_NVS_NO_FREE_PAGES || err == ESP_ERR_NVS_NEW_VERSION_FOUND) {
		ESP_ERROR_CHECK(nvs_flash_erase());
		err = nvs_flash_init();
	}
	ESP_ERROR_CHECK(err);
	ESP_ERROR_CHECK(esp_event_loop_create_default());
	wifi_init_config_t config = WIFI_INIT_CONFIG_DEFAULT();
	ESP_ERROR_CHECK(esp_wifi_init(&config));
	ESP_ERROR_CHECK(esp_wifi_set_storage(WIFI_STORAGE_RAM));
	ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_STA));
#if CONFIG_IDF_TARGET_ESP32C5
	ESP_ERROR_CHECK(configure_c5_tx_rate());
#endif
	ESP_ERROR_CHECK(esp_wifi_start());
#if CONFIG_IDF_TARGET_ESP32C5
	ESP_ERROR_CHECK(esp_wifi_set_band_mode(WIFI_BAND_MODE_2G_ONLY));
#endif
	ESP_ERROR_CHECK(esp_wifi_set_ps(WIFI_PS_NONE));
	ESP_ERROR_CHECK(esp_wifi_get_mac(WIFI_IF_STA, mac));
	ESP_ERROR_CHECK(esp_wifi_register_80211_tx_cb(tx_done_cb));
}

static esp_err_t set_channel_for_band(uint8_t channel)
{
#if CONFIG_IDF_TARGET_ESP32C5
	if (channel != 6 && channel != 44 && channel != 149) return ESP_ERR_INVALID_ARG;
	wifi_band_mode_t next = channel == 6 ? WIFI_BAND_MODE_2G_ONLY : WIFI_BAND_MODE_5G_ONLY;
	wifi_band_mode_t previous = current_channel == 6 ? WIFI_BAND_MODE_2G_ONLY
	                                                 : WIFI_BAND_MODE_5G_ONLY;
	bool changed = next != previous;
	bool resume_promiscuous = c5_promiscuous_enabled;
	esp_err_t err = changed && resume_promiscuous ? c5_set_promiscuous(false) : ESP_OK;
	if (err != ESP_OK) return err;
	if (changed) {
		err = esp_wifi_set_band_mode(next);
		if (err != ESP_OK) {
			if (resume_promiscuous) (void)c5_set_promiscuous(true);
			return err;
		}
	}
	err = esp_wifi_set_channel(channel, WIFI_SECOND_CHAN_NONE);
	if (err != ESP_OK && changed) {
		(void)esp_wifi_set_band_mode(previous);
		(void)esp_wifi_set_channel(current_channel, WIFI_SECOND_CHAN_NONE);
	}
	if (changed && resume_promiscuous) {
		esp_err_t resume = c5_set_promiscuous(true);
		if (resume != ESP_OK) {
			if (err == ESP_OK) {
				(void)esp_wifi_set_band_mode(previous);
				(void)esp_wifi_set_channel(current_channel, WIFI_SECOND_CHAN_NONE);
			}
			err = resume;
		}
	}
	return err;
#else
	if (channel > 14) return ESP_ERR_INVALID_ARG;
	return esp_wifi_set_channel(channel, WIFI_SECOND_CHAN_NONE);
#endif
}

int32_t owl_hal_apply(uint8_t channel, uint8_t filter)
{
	const wifi_promiscuous_filter_t f = {
		.filter_mask = WIFI_PROMIS_FILTER_MASK_MGMT | WIFI_PROMIS_FILTER_MASK_DATA |
		               (filter == FILTER_ALL || owl_hal_ba_enabled() ? WIFI_PROMIS_FILTER_MASK_CTRL : 0),
	};
	const wifi_promiscuous_filter_t cf = {
		.filter_mask = filter == FILTER_ALL ? WIFI_PROMIS_CTRL_FILTER_MASK_ALL : WIFI_PROMIS_CTRL_FILTER_MASK_BAR,
	};
	ESP_ERROR_CHECK(esp_wifi_set_promiscuous_filter(&f));
	ESP_ERROR_CHECK(esp_wifi_set_promiscuous_ctrl_filter(&cf));
	ESP_ERROR_CHECK(esp_wifi_set_promiscuous_rx_cb(promisc_cb));
	esp_err_t channel_error;
#if CONFIG_IDF_TARGET_ESP32C5
	channel_error = set_channel_for_band(channel);
	if (channel_error == ESP_OK && !c5_promiscuous_enabled) {
		channel_error = c5_set_promiscuous(true);
	}
	#else
	ESP_ERROR_CHECK(esp_wifi_set_promiscuous(true));
	channel_error = set_channel_for_band(channel);
#endif
	if (channel_error == ESP_OK) {
	#if CONFIG_IDF_TARGET_ESP32C5
		current_channel = channel;
	#else
		current_rate = -1;
	#endif
	}
	return channel_error;
}

int32_t owl_hal_set_channel(uint8_t channel)
{
	esp_err_t channel_error = set_channel_for_band(channel);
	if (channel_error == ESP_OK) {
	#if CONFIG_IDF_TARGET_ESP32C5
		current_channel = channel;
	#else
		current_rate = -1;
	#endif
	}
	return channel_error;
}

int32_t owl_hal_set_mac(const uint8_t mac[6], uint8_t actual_mac[6], uint8_t channel, uint8_t filter)
{
#if CONFIG_IDF_TARGET_ESP32C5
	if (c5_promiscuous_enabled) (void)c5_set_promiscuous(false);
#else
	esp_wifi_set_promiscuous(false);
#endif
	esp_wifi_stop();
	esp_err_t err = esp_wifi_set_mac(WIFI_IF_STA, mac);
#if CONFIG_IDF_TARGET_ESP32C5
	ESP_ERROR_CHECK(configure_c5_tx_rate());
#else
	current_rate = -1; /* Restarting Wi-Fi resets the driver's rate selection. */
#endif
	ESP_ERROR_CHECK(esp_wifi_start());
#if CONFIG_IDF_TARGET_ESP32C5
	ESP_ERROR_CHECK(esp_wifi_set_band_mode(current_channel == 6 ? WIFI_BAND_MODE_2G_ONLY
	                                                          : WIFI_BAND_MODE_5G_ONLY));
#endif
	ESP_ERROR_CHECK(esp_wifi_set_ps(WIFI_PS_NONE));
	ESP_ERROR_CHECK(esp_wifi_get_mac(WIFI_IF_STA, actual_mac));
	esp_err_t apply_error = owl_hal_apply(channel, filter);
	return err == ESP_OK ? apply_error : err;
}

uint8_t owl_hal_default_channel(void)
{
#if CONFIG_IDF_TARGET_ESP32C5
	return OWL_C5_DEFAULT_CHANNEL;
#else
	return 6;
#endif
}

/* Portable protocol rate -> chip-specific IDF enum. */
#if !CONFIG_IDF_TARGET_ESP32C5
static int phy_rate(uint8_t rate)
{
	if (rate & RATE_MCS) {
		int mcs = rate & 0x0f;
		if (mcs > 7) return -1;
		return (rate & RATE_MCS_SGI ? WIFI_PHY_RATE_MCS0_SGI : WIFI_PHY_RATE_MCS0_LGI) + mcs;
	}
	switch (rate) {
	case 2: return WIFI_PHY_RATE_1M_L;
	case 4: return WIFI_PHY_RATE_2M_L;
	case 11: return WIFI_PHY_RATE_5M_L;
	case 22: return WIFI_PHY_RATE_11M_L;
	case 12: return WIFI_PHY_RATE_6M;
	case 18: return WIFI_PHY_RATE_9M;
	case 24: return WIFI_PHY_RATE_12M;
	case 36: return WIFI_PHY_RATE_18M;
	case 48: return WIFI_PHY_RATE_24M;
	case 72: return WIFI_PHY_RATE_36M;
	case 96: return WIFI_PHY_RATE_48M;
	case 108: return WIFI_PHY_RATE_54M;
	default: return -1;
	}
}
#endif

int32_t owl_hal_transmit(uint8_t rate, const uint8_t *frame, size_t len)
{
#if CONFIG_IDF_TARGET_ESP32C5
	/* C5 uses its fixed HE20/MCS0 profile configured before Wi-Fi starts. IDF's
	 * raw-TX API limits frames to 1500 bytes. */
	(void)rate;
	if (len < 24 || len > 1500) return ESP_ERR_INVALID_ARG;
	return esp_wifi_80211_tx(WIFI_IF_STA, frame, len, false);
#else
	int phy = phy_rate(rate);
	if (phy < 0) return ESP_ERR_INVALID_ARG;
	if (phy != current_rate) {
		esp_err_t err = esp_wifi_config_80211_tx_rate(WIFI_IF_STA, (wifi_phy_rate_t)phy);
		current_rate = err == ESP_OK ? phy : -1;
		if (err != ESP_OK) return err;
	}
	return esp_wifi_80211_tx(WIFI_IF_STA, frame, len, false);
#endif
}

size_t owl_hal_read(uint8_t *out, size_t cap)
{
#if CONFIG_OWL_RADIO_LINK_USB_SERIAL_JTAG
	int got = usb_serial_jtag_read_bytes(out, cap, portMAX_DELAY);
	return got > 0 ? (size_t)got : 0;
#else
	if (!cap || uart_read_bytes(UART_NUM_0, out, 1, portMAX_DELAY) != 1) return 0;
	size_t more = 0;
	uart_get_buffered_data_len(UART_NUM_0, &more);
	if (more > cap - 1) more = cap - 1;
	int got = more ? uart_read_bytes(UART_NUM_0, out + 1, more, 0) : 0;
	return 1 + (got > 0 ? (size_t)got : 0);
#endif
}

size_t owl_hal_write(const uint8_t *data, size_t len)
{
	size_t total = len;
	while (len) {
#if CONFIG_OWL_RADIO_LINK_USB_SERIAL_JTAG
		int sent = usb_serial_jtag_write_bytes(data, len, portMAX_DELAY);
#else
		int sent = uart_write_bytes(UART_NUM_0, data, len);
#endif
		if (sent <= 0 || (size_t)sent > len) break;
		data += sent;
		len -= (size_t)sent;
	}
	return total - len;
}

bool owl_hal_enqueue(uint8_t kind, const uint8_t *header, size_t header_len, const uint8_t *frame, size_t len)
{
	void *slot;
	if (xRingbufferSendAcquire(rx_ring, &slot, 1 + header_len + len, 0) != pdTRUE) return false;
	((uint8_t *)slot)[0] = kind;
	memcpy((uint8_t *)slot + 1, header, header_len);
	memcpy((uint8_t *)slot + 1 + header_len, frame, len);
	xRingbufferSendComplete(rx_ring, slot);
	return true;
}

const uint8_t *owl_hal_dequeue(size_t *len, bool wait) {
	return xRingbufferReceive(rx_ring, len, wait ? portMAX_DELAY : 0);
}
void owl_hal_release(const uint8_t *item) { vRingbufferReturnItem(rx_ring, (void *)item); }
uint64_t owl_hal_time_us(void) { return (uint64_t)esp_timer_get_time(); }
void owl_hal_delay_ms(uint32_t ms) { vTaskDelay(pdMS_TO_TICKS(ms)); }
const char *owl_hal_target(void) { return CONFIG_IDF_TARGET; }
const char *owl_hal_idf_version(void) { return esp_get_idf_version(); }
uint32_t owl_hal_heap_free(void) { return heap_caps_get_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT); }
uint32_t owl_hal_heap_minimum(void) { return heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT); }
uint32_t owl_hal_link_core(void) { return link_core; }

void owl_hal_start_tasks(void)
{
	if (owl_hal_ba_enabled()) {
		ESP_ERROR_CHECK(xTaskCreatePinnedToCore(owl_radio_aggregation_task, "rx_ba", 4096, NULL, 12, NULL,
		                                      tskNO_AFFINITY) == pdPASS ? ESP_OK : ESP_ERR_NO_MEM);
	}
	ESP_ERROR_CHECK(xTaskCreatePinnedToCore(owl_radio_uplink_task, "uplink", 4096, NULL, 10, NULL,
	                                      OWL_LINK_CORE) == pdPASS ? ESP_OK : ESP_ERR_NO_MEM);
	ESP_ERROR_CHECK(xTaskCreatePinnedToCore(owl_radio_command_task, "command", 6144, NULL, 11, NULL,
	                                      tskNO_AFFINITY) == pdPASS ? ESP_OK : ESP_ERR_NO_MEM);
#if CONFIG_OWL_CARDPUTER_UI
	ESP_ERROR_CHECK(xTaskCreatePinnedToCore(owl_radio_ui_task, "cardputer_ui", 10240, NULL, 3, NULL,
	                                      tskNO_AFFINITY) == pdPASS ? ESP_OK : ESP_ERR_NO_MEM);
#endif
}

void app_main(void) { owl_radio_main(); }
