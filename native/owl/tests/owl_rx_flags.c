/* Based on t4t5/omdrop-owl@8213aee tests/owl_rx_flags.c (see native/owl/patches/README.md).
 * Verify absent/present metadata and the Windows host-clock TSFT extension.
 * The test library uses pattern initialization to expose missing defaults. */
#include <stdio.h>
#include <string.h>
#include "rx.h"
#include "state.h"
#include "tx.h"
#include "wire.h"
#include "radiotap.h"

static const struct ether_addr self = {{2, 0, 0, 0, 0, 1}}, peer = {{2, 0, 0, 0, 0, 2}};

static int check(const char *what, int has_flags, uint8_t flags, int has_signal, signed char signal,
                 int has_tsft, uint64_t tsft, int host_clock, int expected) {
	struct awdl_state tx = {0}, rx = {0};
	struct ieee80211_state ieee = {0};
	struct buf *output[1], **next = output;
	struct awdl_peer *p = NULL;
	uint8_t original[1024], raw[1024] = {0};
	awdl_init_state(&tx, "test", &peer, CHAN_OPCLASS_6, clock_time_us());
	int len = awdl_init_full_action_frame(original, &tx, &ieee, AWDL_ACTION_MIF);
	int original_rtlen = original[2] | original[3] << 8;
	uint32_t present = htole32((has_tsft ? 1u << IEEE80211_RADIOTAP_TSFT : 0) |
	    (has_flags ? 1u << IEEE80211_RADIOTAP_FLAGS : 0) |
	    (has_signal ? 1u << IEEE80211_RADIOTAP_DBM_ANTSIGNAL : 0));
	memcpy(raw + 4, &present, sizeof present);
	int rtlen = 8;
	if (has_tsft) { uint64_t little = htole64(tsft); memcpy(raw + rtlen, &little, 8); rtlen += 8; }
	if (has_flags) raw[rtlen++] = flags;
	if (has_signal) raw[rtlen++] = (uint8_t) signal;
	raw[2] = (uint8_t) rtlen;
	memcpy(raw + rtlen, original + original_rtlen, len - original_rtlen);
	len += rtlen - original_rtlen;
	if (has_flags && (flags & IEEE80211_RADIOTAP_F_FCS)) len += 4;

	awdl_init_state(&rx, "test", &self, CHAN_OPCLASS_6, clock_time_us());
	rx.rx_tsft_host_clock = host_clock;
	const struct buf *frame = buf_new_const(raw, len);
	uint64_t before = clock_time_us();
	int result = awdl_rx(frame, &next, output + 1, &rx);
	uint64_t after = clock_time_us();
	int found = awdl_peer_get(rx.peers.peers, &peer, &p);
	int time_ok = expected != RX_OK || (found == PEERS_OK &&
	    ((host_clock && has_tsft && tsft) ? p->last_update == tsft :
	        p->last_update >= before && p->last_update <= after));
	buf_free(frame);
	awdl_peers_free(tx.peers.peers);
	awdl_peers_free(rx.peers.peers);
	if (result != expected || !time_ok || next != output) {
		fprintf(stderr, "%s: result %d (want %d), timestamp valid %d\n", what, result, expected, time_ok);
		return 1;
	}
	return 0;
}

int main(void) {
	int failed = 0;
	failed |= check("no optional fields", 0, 0, 0, 0, 0, 0, 0, RX_OK);
	failed |= check("explicit flags", 1, 0, 1, -40, 0, 0, 0, RX_OK);
	failed |= check("bad FCS", 1, IEEE80211_RADIOTAP_F_BADFCS, 1, -40, 0, 0, 0, RX_IGNORE_FAILED_CRC);
	failed |= check("strip FCS trailer", 1, IEEE80211_RADIOTAP_F_FCS, 1, -40, 0, 0, 0, RX_OK);
	failed |= check("weak signal still rejected", 0, 0, 1, -100, 0, 0, 0, RX_IGNORE_RSSI);
	uint64_t old = clock_time_us() - 1000000;
	failed |= check("host TSFT with -T", 1, 0, 1, -40, 1, old, 1, RX_OK);
	failed |= check("foreign TSFT without -T", 1, 0, 1, -40, 1, old, 0, RX_OK);
	failed |= check("missing TSFT with -T", 0, 0, 0, 0, 0, 0, 1, RX_OK);
	failed |= check("zero TSFT with -T", 0, 0, 0, 0, 1, 0, 1, RX_OK);
	return failed;
}
