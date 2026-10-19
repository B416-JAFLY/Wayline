#include "radio_ui.h"

#include <M5Unified.hpp>
#include "nvs.h"
#include "esp_timer.h"
#include <atomic>

namespace {

constexpr uint8_t TCA8418_ADDR = 0x34;
constexpr uint32_t I2C_HZ = 100000;
constexpr uint8_t REG_CFG = 0x01;
constexpr uint8_t REG_KEY_LCK_EC = 0x03;
constexpr uint8_t REG_KEY_EVENT_A = 0x04;
constexpr uint8_t REG_KP_GPIO_1 = 0x1d;
constexpr uint8_t REG_KP_GPIO_2 = 0x1e;
constexpr uint8_t REG_KP_GPIO_3 = 0x1f;
constexpr uint8_t REG_GPI_EM_1 = 0x20;
constexpr uint8_t REG_GPI_EM_2 = 0x21;
constexpr uint8_t REG_GPI_EM_3 = 0x22;
constexpr uint8_t REG_GPIO_DIR_1 = 0x23;
constexpr uint8_t REG_GPIO_DIR_2 = 0x24;
constexpr uint8_t REG_GPIO_DIR_3 = 0x25;
constexpr uint8_t REG_GPIO_INT_LVL_1 = 0x26;
constexpr uint8_t REG_GPIO_INT_LVL_2 = 0x27;
constexpr uint8_t REG_GPIO_INT_LVL_3 = 0x28;
constexpr uint8_t REG_GPIO_INT_EN_1 = 0x1a;
constexpr uint8_t REG_GPIO_INT_EN_2 = 0x1b;
constexpr uint8_t REG_GPIO_INT_EN_3 = 0x1c;


bool s_keyboard_ready;
bool s_fn_down;
bool s_shift_down;
uint8_t s_brightness = 180;
bool s_speaker_ready;
std::atomic<uint32_t> s_ui_debug{0};
int64_t s_keyboard_retry_at;
uint8_t s_keyboard_failed_reg;

void update_debug_state()
{
    s_ui_debug.store(4 | (uint32_t(s_keyboard_ready) << 8)
        | (uint32_t(s_speaker_ready) << 9)
        | (uint32_t(M5.In_I2C.getSDA() == 8 && M5.In_I2C.getSCL() == 9) << 10)
        | (uint32_t(M5.getBoard()) << 16) | (uint32_t(s_keyboard_failed_reg) << 24));
}

bool write_reg(uint8_t reg, uint8_t value)
{
	bool ok = M5.In_I2C.writeRegister8(TCA8418_ADDR, reg, value, I2C_HZ);
    if (!ok && !s_keyboard_failed_reg) s_keyboard_failed_reg = reg;
    return ok;
}

uint8_t read_reg(uint8_t reg)
{
	return M5.In_I2C.readRegister8(TCA8418_ADDR, reg, I2C_HZ);
}

bool setup_keyboard()
{
    s_keyboard_retry_at = esp_timer_get_time() + 500000;
    s_keyboard_failed_reg = 0;
    // This build targets Cardputer ADV. Display autodetection can retain an
    // older Cardputer pin map; its keyboard has no internal I2C bus.
    if (M5.In_I2C.getSDA() != 8 || M5.In_I2C.getSCL() != 9) {
        if (!M5.In_I2C.begin(I2C_NUM_1, 8, 9)) return false;
    }

	bool ok = true;
	ok &= write_reg(REG_GPIO_DIR_1, 0x00);
	ok &= write_reg(REG_GPIO_DIR_2, 0x00);
	ok &= write_reg(REG_GPIO_DIR_3, 0x00);
	ok &= write_reg(REG_GPI_EM_1, 0x00);
	ok &= write_reg(REG_GPI_EM_2, 0x00);
	ok &= write_reg(REG_GPI_EM_3, 0x00);
	ok &= write_reg(REG_GPIO_INT_LVL_1, 0x00);
	ok &= write_reg(REG_GPIO_INT_LVL_2, 0x00);
	ok &= write_reg(REG_GPIO_INT_LVL_3, 0x00);
	ok &= write_reg(REG_GPIO_INT_EN_1, 0x00);
	ok &= write_reg(REG_GPIO_INT_EN_2, 0x00);
	ok &= write_reg(REG_GPIO_INT_EN_3, 0x00);
	ok &= write_reg(REG_KP_GPIO_1, 0x7f); /* seven rows */
	ok &= write_reg(REG_KP_GPIO_2, 0xff); /* eight columns */
	ok &= write_reg(REG_KP_GPIO_3, 0x00);
	ok &= write_reg(REG_CFG, 0x00); /* poll the event FIFO without using GPIO IRQ */
	/* Bound the flush so a disconnected/faulty I2C device cannot stall the UI task. */
	for (unsigned i = 0; i < 16 && (read_reg(REG_KEY_LCK_EC) & 0x0f); ++i)
		(void)read_reg(REG_KEY_EVENT_A);
	return ok;
}

/* TCA8418 key positions mapped to the Cardputer's 4 x 14 keyboard layout. */
bool decode_key(uint8_t event, char *key, bool *pressed)
{
	if (event == 0)
		return false;
	/* TCA8418 bit 7 is set for key-down (M5Cardputer's update_key_list contract). */
	*pressed = (event & 0x80) != 0;
	uint8_t pos = (event & 0x7f) - 1;
	uint8_t raw_row = pos / 10;
	uint8_t raw_col = pos % 10;
	uint8_t row = (raw_col + 4) % 4;
	uint8_t col = raw_row * 2 + (raw_col > 3 ? 1 : 0);

	if (row == 2 && col == 0) {
		s_fn_down = *pressed;
		return false;
	}
	if (row == 2 && col == 1) {
		s_shift_down = *pressed;
		return false;
	}
	if (!*pressed)
		return false;

	*key = 0;
	if (row == 0) {
		static constexpr char keys[] = "`1234567890-=";
		if (col < sizeof(keys) - 1)
			*key = keys[col];
		else if (col == 13)
			*key = '\b';
	} else if (row == 1) {
		static constexpr char keys[] = "qwertyuiop[]\\";
		if (col == 0)
			*key = '\t';
		else if (col <= 13)
			*key = keys[col - 1];
	} else if (row == 2) {
		static constexpr char keys[] = "asdfghjkl;'";
		if (col >= 2 && col <= 12)
			*key = keys[col - 2];
		else if (col == 13)
			*key = '\n';
	} else if (row == 3) {
		static constexpr char keys[] = "zxcvbnm,./";
		if (col >= 3 && col <= 12)
			*key = keys[col - 3];
		else if (col == 13)
			*key = ' ';
	}

	if (*key && s_shift_down && *key >= 'a' && *key <= 'z')
		*key = (char)(*key - 'a' + 'A');
	if (s_fn_down && *key == ',') *key = '<';
	if (s_fn_down && *key == '.') *key = '>';
	return *key != 0;
}

} // namespace

extern "C" void radio_ui_start(void)
{
    s_ui_debug.store(1);
    auto cfg = M5.config();
    cfg.clear_display = true;
    cfg.internal_imu = false;
    cfg.internal_rtc = false;
    cfg.internal_mic = false;
    cfg.internal_spk = true;
    cfg.led_brightness = 0;
    M5.begin(cfg);
    s_ui_debug.store(2);
    s_keyboard_ready = setup_keyboard();
    auto speaker = M5.Speaker.config();
    speaker.task_priority = 2; // Below the UI and USB/radio tasks.
    M5.Speaker.config(speaker);
    M5.Speaker.setVolume(0);
    s_speaker_ready = M5.Speaker.begin();
    s_ui_debug.store(3);
    M5.Display.setBrightness(s_brightness);
    // The Rust framebuffer contains native-endian RGB565 values.
    M5.Display.setSwapBytes(true);
    update_debug_state();
}

extern "C" uint32_t radio_ui_debug_state(void) { return s_ui_debug.load(); }

extern "C" bool radio_ui_poll_key(char *key, bool *pressed)
{
    if (!s_keyboard_ready) {
        if (esp_timer_get_time() < s_keyboard_retry_at) return false;
        s_keyboard_ready = setup_keyboard();
        s_fn_down = s_shift_down = false;
        update_debug_state();
        if (!s_keyboard_ready) return false;
    }
    uint8_t count = 0, event = 0;
    if (!M5.In_I2C.readRegister(TCA8418_ADDR, REG_KEY_LCK_EC, &count, 1, I2C_HZ)
        || ((count & 0x0f) && !M5.In_I2C.readRegister(TCA8418_ADDR, REG_KEY_EVENT_A, &event, 1, I2C_HZ))) {
        s_keyboard_ready = false;
        s_keyboard_retry_at = esp_timer_get_time() + 500000;
        update_debug_state();
        return false;
    }
    if ((count & 0x0f) == 0)
        return false;
    return decode_key(event, key, pressed);
}

extern "C" void radio_ui_set_brightness(uint8_t brightness)
{
    s_brightness = brightness;
    M5.Display.setBrightness(brightness);
}

extern "C" void radio_ui_set_volume(uint8_t volume)
{
    M5.Speaker.setVolume(volume);
    if (s_speaker_ready && volume == 0) M5.Speaker.stop();
}

extern "C" void radio_ui_stop_sound(void)
{
    if (s_speaker_ready) M5.Speaker.stop();
}

extern "C" bool radio_ui_tone(uint16_t frequency, uint16_t duration_ms,
                              const uint8_t *samples, size_t len)
{
    // Auto channel never waits for a queue slot. Rust owns static unsigned 8-bit waveforms.
    return s_speaker_ready && M5.Speaker.getVolume() != 0 && samples && len == 32
        && M5.Speaker.tone(frequency, duration_ms, -1, true, samples, len);
}

extern "C" void radio_ui_present(const uint16_t *pixels)
{
    // One complete offscreen frame per SPI transaction, with no intermediate clear.
    M5.Display.pushImage(0, 0, 240, 135, pixels);
}

extern "C" uint32_t radio_ui_load_preferences(void)
{
    nvs_handle_t handle;
    uint32_t value = 0; // Rust validates the version/fields and supplies defaults.
    if (nvs_open("omdrop-ui", NVS_READONLY, &handle) == ESP_OK) {
        (void)nvs_get_u32(handle, "preferences", &value);
        nvs_close(handle);
    }
    return value;
}

extern "C" bool radio_ui_save_preferences(uint32_t preferences)
{
    nvs_handle_t handle;
    if (nvs_open("omdrop-ui", NVS_READWRITE, &handle) != ESP_OK)
        return false;
    esp_err_t error = nvs_set_u32(handle, "preferences", preferences);
    if (error == ESP_OK)
        error = nvs_commit(handle);
    nvs_close(handle);
    return error == ESP_OK;
}
