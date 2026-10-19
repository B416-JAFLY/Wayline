/* Exercise the real Windows queue and OWL core without opening hardware. Including
 * the backend keeps its queue internals private to production callers. */
#include "../io_esp.c"
#include "core.h"
#include "tx.h"
#include "schedule.h"

#define CHECK(expr) do { if (!(expr)) { fprintf(stderr, "line %d: %s\n", __LINE__, #expr); return 1; } } while (0)

static void drain_queue(void) {
	EnterCriticalSection(&radio.tx_lock);
	radio.tx_head = radio.tx_count = 0;
	LeaveCriticalSection(&radio.tx_lock);
	ResetEvent(radio.tx_wake);
}

int main(void) {
	const struct ether_addr self = {{2, 0, 0, 0, 0, 1}}, peer = {{2, 0, 0, 0, 0, 2}};
	struct daemon_state state = {0};
	struct awdl_peer *p = NULL;
	uint8_t ethernet[18] = {0};
	memcpy(ethernet, &peer, 6);
	memcpy(ethernet + 6, &self, 6);
	ethernet[12] = 0x86; ethernet[13] = 0xdd;
	memcpy(ethernet + 14, "test", 4);
	const struct buf *frame = buf_new_const(ethernet, sizeof ethernet);

	InitializeCriticalSection(&radio.tx_lock);
	radio.tx_wake = CreateEventW(NULL, FALSE, FALSE, NULL);
	radio.reply_event = CreateEventW(NULL, TRUE, FALSE, NULL);
	radio.txq = calloc(TXQ_LEN, sizeof *radio.txq);
	CHECK(radio.tx_wake && radio.reply_event && radio.txq);
	state.io.wlan_handle = &radio_pcap;
	awdl_init_state(&state.awdl_state, "test", &self, CHAN_OPCLASS_6, clock_time_us());
	state.awdl_state.channel.fixed_radio = 1;
	CHECK(awdl_peer_add(state.awdl_state.peers.peers, &peer, clock_time_us(), NULL, NULL) == PEERS_OK);
	CHECK(awdl_peer_get(state.awdl_state.peers.peers, &peer, &p) == PEERS_OK);
	p->sequence_enc = AWDL_CHAN_ENC_OPCLASS;
	for (int i = 0; i < AWDL_CHANSEQ_LENGTH; i++) p->sequence[i] = CHAN_OPCLASS_6;
	CHECK(awdl_can_send_unicast_in(&state.awdl_state, p, clock_time_us(), AWDL_UNICAST_GUARD_TU) == 0);

	for (int i = 0; i < TXQ_LEN; i++) CHECK(radio_queue(MSG_TX, ethernet, sizeof ethernet, NULL, 0) == 0);
	CHECK(awdl_send_data(frame, &state.io, &state.awdl_state, &state.ieee80211_state) == TX_AGAIN);
	CHECK(radio.tx_count == TXQ_LEN && state.awdl_state.stats.tx_data == 0);
	CHECK(buf_len(frame) == sizeof ethernet && !memcmp(buf_data(frame), ethernet, sizeof ethernet));
	CHECK(radio_queue(MSG_TX, ethernet, SIZE_MAX, NULL, 0) == -EMSGSIZE);
	CHECK(radio_request(MSG_HELLO, NULL, 0, MSG_HELLO_RESP, 1) == -EAGAIN);
	CHECK(radio.want_type == -1);

	struct ev_loop *loop = EV_DEFAULT;
	ev_timer_init(&state.ev_state.tx_timer, awdl_send_unicast, 0, 0);
	state.ev_state.tx_timer.data = &state;
	state.next = (struct buf *) frame;
	state.next_since = clock_time_us();
	awdl_send_unicast(loop, &state.ev_state.tx_timer, 0);
	CHECK(state.next == frame && state.ev_state.tx_timer.active);
	CHECK(state.ev_state.tx_timer.after == 0.0005);
	CHECK(buf_len(frame) == sizeof ethernet && !memcmp(buf_data(frame), ethernet, sizeof ethernet));

	drain_queue();
	for (int i = 0; i < AWDL_CHANSEQ_LENGTH; i++) p->sequence[i] = CHAN_OPCLASS_149;
	awdl_send_unicast(loop, &state.ev_state.tx_timer, 0);
	CHECK(state.next == frame && radio.tx_count == 0);
	for (int i = 0; i < AWDL_CHANSEQ_LENGTH; i++) p->sequence[i] = CHAN_OPCLASS_6;
	awdl_send_unicast(loop, &state.ev_state.tx_timer, 0);
	CHECK(state.next == NULL && radio.tx_count == 1);
	CHECK(state.awdl_state.stats.tx_data == 1 && state.awdl_state.stats.tx_data_unicast == 1);
	uint8_t decoded[COBS_MAX];
	memcpy(decoded, radio.txq[0].data, radio.txq[0].len);
	int n = cobs_decode(decoded, radio.txq[0].len - 1);
	CHECK(n > 2 && decoded[0] == MSG_TX);
	CHECK(crc16(decoded, n - 2) == (uint16_t) (decoded[n - 2] | decoded[n - 1] << 8));
	CHECK(!memcmp(decoded + n - 2 - 4, "test", 4));

	// A peer on another channel still cannot block all later frames indefinitely.
	state.next = (struct buf *) buf_new_const(ethernet, sizeof ethernet);
	state.next_since = clock_time_us() - 2000001;
	for (int i = 0; i < AWDL_CHANSEQ_LENGTH; i++) p->sequence[i] = CHAN_OPCLASS_149;
	awdl_send_unicast(loop, &state.ev_state.tx_timer, 0);
	CHECK(state.next == NULL && radio.tx_count == 1);

	uint8_t bad_rt[8] = {0};
	CHECK(wlan_send(&state.io, bad_rt, sizeof bad_rt) == -EINVAL);
	CHECK(wlan_send(&state.io, NULL, 8) == -EINVAL);
	CHECK(wlan_send(&state.io, bad_rt, 4) == -EINVAL);
	ev_timer_stop(loop, &state.ev_state.tx_timer);

	/* A failed serial write must wake the main loop and reject further frames,
	 * allowing OWL to exit and the supervisor to reopen the device. */
	drain_queue();
	radio.rx_ready = CreateEventW(NULL, TRUE, FALSE, NULL);
	radio.com = INVALID_HANDLE_VALUE;
	CHECK(radio_queue(MSG_TX, ethernet, sizeof ethernet, NULL, 0) == 0);
	writer_main(NULL);
	CHECK(radio.failed && radio.tx_count == 0);
	CHECK(WaitForSingleObject(radio.rx_ready, 0) == WAIT_OBJECT_0);
	CHECK(radio_queue(MSG_TX, ethernet, sizeof ethernet, NULL, 0) == -EIO);
	CHECK(pcap_dispatch(&radio_pcap, 1, NULL, NULL) == -1);
	CloseHandle(radio.rx_ready);
	awdl_peers_free(state.awdl_state.peers.peers);
	free(radio.txq);
	CloseHandle(radio.tx_wake); CloseHandle(radio.reply_event);
	DeleteCriticalSection(&radio.tx_lock);
	return 0;
}
