/* Exercise the real queue/writer with deterministic short and pending writes.
 * No serial port, radio or Wintun session is opened. */
#include <windows.h>
static BOOL WINAPI mock_write(HANDLE, LPCVOID, DWORD, LPDWORD, LPOVERLAPPED);
static BOOL WINAPI mock_result(HANDLE, LPOVERLAPPED, LPDWORD, BOOL);
#define WriteFile mock_write
#define GetOverlappedResult mock_result
#include "../io_esp.c"
#undef WriteFile
#undef GetOverlappedResult

#define CHECK(expr) do { if (!(expr)) { fprintf(stderr, "line %d: %s\n", __LINE__, #expr); return 1; } } while (0)
static uint8_t expected[TXQ_LEN * COBS_MAX], received[TXQ_LEN * COBS_MAX];
static size_t expected_len, received_len;
static DWORD write_limit, pending_count, max_request;
static int pending_mode, write_calls, fail_after;

static BOOL WINAPI mock_write(HANDLE h, LPCVOID data, DWORD count, LPDWORD done, LPOVERLAPPED ov)
{
	(void) h; (void) ov;
	write_calls++;
	if (count > max_request) max_request = count;
	if (fail_after && write_calls >= fail_after) {
		*done = 0;
		SetLastError(ERROR_WRITE_FAULT);
		return FALSE;
	}
	DWORD n = count < write_limit ? count : write_limit;
	if (received_len + n > sizeof received) abort();
	memcpy(received + received_len, data, n);
	received_len += n;
	if (received_len == expected_len) radio.stop = 1;
	if (pending_mode) {
		pending_count = n;
		*done = 0;
		SetLastError(ERROR_IO_PENDING);
		return FALSE;
	}
	*done = n;
	return TRUE;
}

static BOOL WINAPI mock_result(HANDLE h, LPOVERLAPPED ov, LPDWORD done, BOOL wait)
{
	(void) h; (void) ov; (void) wait;
	*done = pending_count;
	return TRUE;
}

static uint16_t reference_crc(const uint8_t *p, size_t n)
{
	uint16_t crc = 0xffff;
	while (n--) {
		crc ^= (uint16_t) *p++ << 8;
		for (int i = 0; i < 8; i++) crc = crc & 0x8000 ? (crc << 1) ^ 0x1021 : crc << 1;
	}
	return crc;
}

static size_t reference_encode(uint8_t type, const uint8_t *body, size_t n, uint8_t *out)
{
	uint8_t raw[RAW_MAX];
	raw[0] = type;
	if (n) memcpy(raw + 1, body, n);
	uint16_t crc = reference_crc(raw, n + 1);
	raw[n + 1] = (uint8_t) crc;
	raw[n + 2] = (uint8_t) (crc >> 8);
	size_t code_at = 0, written = 1;
	uint8_t code = 1;
	for (size_t i = 0; i < n + 3; i++) {
		if (raw[i]) { out[written++] = raw[i]; code++; }
		if (!raw[i] || code == 0xff) {
			out[code_at] = code; code_at = written++; code = 1;
		}
	}
	out[code_at] = code;
	out[written++] = 0;
	return written;
}

static int codec_test(void)
{
	uint8_t body[RAW_MAX - 3], old[COBS_MAX], current[COBS_MAX + 1];
	CHECK(crc16((const uint8_t *) "123456789", 9) == 0x29b1);
	for (unsigned i = 0; i <= UINT16_MAX; i++) {
		uint8_t bytes[2] = { (uint8_t) i, (uint8_t) (i >> 8) };
		CHECK(crc16(bytes, 2) == reference_crc(bytes, 2));
	}
	for (int pattern = 0; pattern < 3; pattern++) {
		for (size_t i = 0; i < sizeof body; i++) body[i] = pattern == 0 ? 0 : pattern == 1 ? 255 : (uint8_t) (i * 131 + i / 7);
		for (size_t n = 0; n <= sizeof body; n++) {
			size_t old_len = reference_encode(MSG_TX, body, n, old);
			current[COBS_MAX] = 0xa5;
			size_t len = radio_encode(MSG_TX, body, n / 2, body + n / 2, n - n / 2, current);
			CHECK(len == old_len && !memcmp(old, current, len));
			CHECK(current[COBS_MAX] == 0xa5);
		}
	}
	return 0;
}

static int fill_queue(size_t payload)
{
	uint8_t body[PROTO_MAX_FRAME];
	expected_len = received_len = 0;
	write_calls = 0; max_request = 0;
	radio.tx_head = TXQ_LEN - 3; radio.tx_count = 0; // exercise wraparound
	radio.stop = radio.failed = 0;
	for (int i = 0; i < TXQ_LEN; i++) {
		memset(body, i, payload);
		/* Include control messages: batching must preserve their FIFO order. */
		uint8_t type = i % 5 ? MSG_TX : MSG_SET_CHANNEL;
		size_t size = type == MSG_TX ? payload : 1;
		CHECK(radio_queue(type, body, size, NULL, 0) == 0);
		expected_len += reference_encode(type, body, size, expected + expected_len);
	}
	CHECK(radio_queue(MSG_TX, body, payload, NULL, 0) == -EAGAIN);
	return 0;
}

static int writer_test(void)
{
	InitializeCriticalSection(&radio.tx_lock);
	radio.txq = calloc(TXQ_LEN, sizeof *radio.txq);
	radio.tx_wake = CreateEventW(NULL, FALSE, FALSE, NULL);
	radio.rx_ready = CreateEventW(NULL, TRUE, FALSE, NULL);
	radio.reply_event = CreateEventW(NULL, TRUE, FALSE, NULL);
	CHECK(radio.txq && radio.tx_wake && radio.rx_ready && radio.reply_event);
	for (int pending = 0; pending < 2; pending++) {
		for (int short_write = 0; short_write < 2; short_write++) {
			for (int large = 0; large < 2; large++) {
				CHECK(fill_queue(large ? PROTO_MAX_FRAME : 31) == 0);
				pending_mode = pending; write_limit = short_write ? 17 : MAXDWORD;
				fail_after = 0;
				uint8_t too_small[1];
				CHECK(radio_take_batch(too_small, sizeof too_small) == 0 && radio.tx_count == TXQ_LEN);
				writer_main(NULL);
				CHECK(!radio.failed && radio.tx_count == 0);
				CHECK(received_len == expected_len && !memcmp(received, expected, expected_len));
				CHECK(max_request <= TX_BATCH_BYTES);
				if (!short_write) CHECK(write_calls < TXQ_LEN);
			}
		}
	}
	CHECK(fill_queue(31) == 0);
	pending_mode = 0; write_limit = 17; fail_after = 2;
	writer_main(NULL);
	CHECK(radio.failed && received_len == 17);
	CHECK(WaitForSingleObject(radio.rx_ready, 0) == WAIT_OBJECT_0);
	CHECK(WaitForSingleObject(radio.reply_event, 0) == WAIT_OBJECT_0);
	CHECK(radio_queue(MSG_HELLO, NULL, 0, NULL, 0) == -EIO);
	/* A sparse queue is sent immediately in one call, without waiting for peers. */
	radio.stop = radio.failed = 0; radio.tx_count = 0;
	expected_len = reference_encode(MSG_HELLO, NULL, 0, expected);
	received_len = 0; write_calls = 0; fail_after = 0; write_limit = MAXDWORD;
	CHECK(radio_queue(MSG_HELLO, NULL, 0, NULL, 0) == 0);
	writer_main(NULL);
	CHECK(write_calls == 1 && received_len == expected_len);
	free(radio.txq);
	CloseHandle(radio.tx_wake); CloseHandle(radio.rx_ready); CloseHandle(radio.reply_event);
	DeleteCriticalSection(&radio.tx_lock);
	return 0;
}

static volatile unsigned benchmark_sink;
static void benchmark(void)
{
	uint8_t body[1600], out[COBS_MAX];
	LARGE_INTEGER frequency, begin, end;
	QueryPerformanceFrequency(&frequency);
	for (size_t i = 0; i < sizeof body; i++) body[i] = (uint8_t) (i * 131 + i / 7);
	for (int mode = 0; mode < 2; mode++) {
		double samples[5];
		for (int sample = 0; sample < 5; sample++) {
			QueryPerformanceCounter(&begin);
			for (unsigned i = 0; i < 100000; i++) {
				body[0] = (uint8_t) i;
				size_t len = mode ? radio_encode(MSG_TX, body, 4, body + 4, sizeof body - 4, out)
				                  : reference_encode(MSG_TX, body, sizeof body, out);
				benchmark_sink += out[i % len];
			}
			QueryPerformanceCounter(&end);
			samples[sample] = (sizeof body + 1) * 100000.0 * frequency.QuadPart /
			                  (end.QuadPart - begin.QuadPart) / 1048576.0;
		}
		for (int i = 0; i < 5; i++) for (int j = i + 1; j < 5; j++)
			if (samples[i] > samples[j]) { double temp = samples[i]; samples[i] = samples[j]; samples[j] = temp; }
		printf("%s host encoding median: %.2f MiB/s (1600-byte payload, 5 x 100000 frames)\n",
		       mode ? "new" : "old", samples[2]);
	}
}

int main(int argc, char **argv)
{
	if (codec_test()) return 1;
	if (argc == 2 && !strcmp(argv[1], "--bench")) { benchmark(); return 0; }
	return writer_test();
}
