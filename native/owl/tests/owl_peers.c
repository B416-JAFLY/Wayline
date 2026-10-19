/* Data traffic keeps a known peer alive through gaps in action frames. */
#include <stdio.h>
#include "rx.h"
#include "state.h"
#include "wire.h"

#define CHECK(expr) do { if (!(expr)) { fprintf(stderr, "line %d: %s\n", __LINE__, #expr); return 1; } } while (0)

int main(void) {
	const struct ether_addr self = {{2, 0, 0, 0, 0, 1}}, peer = {{2, 0, 0, 0, 0, 2}};
	const struct ether_addr stranger = {{2, 0, 0, 0, 0, 3}};
	const uint8_t raw[16] = {0xaa, 0xaa, 3, 0, 0x17, 0xf2, 8, 0, 4, 3, 0, 0, 0, 0, 8, 0};
	struct awdl_state state;
	struct awdl_peer *p = NULL;
	struct buf *output[1], **next = output;
	awdl_init_state(&state, "test", &self, CHAN_OPCLASS_6, clock_time_us());
	CHECK(state.peers.timeout == 5000000);
	CHECK(awdl_peer_add(state.peers.peers, &peer, 1, NULL, NULL) == PEERS_OK);
	CHECK(awdl_peer_get(state.peers.peers, &peer, &p) == PEERS_OK);
	uint64_t before = clock_time_us();
	const struct buf *frame = buf_new_const(raw, sizeof raw);
	CHECK(awdl_rx_data(frame, &next, output + 1, &peer, &self, &state) == RX_OK);
	CHECK(next == output + 1);
	CHECK(p->last_update >= before && p->last_update <= clock_time_us());
	buf_free(output[0]);
	buf_free(frame);
	awdl_peers_remove(state.peers.peers, before, NULL, NULL);
	CHECK(awdl_peer_get(state.peers.peers, &peer, &p) == PEERS_OK);
	// The refresh must not disable eviction once the peer stops sending.
	awdl_peers_remove(state.peers.peers, p->last_update + 1, NULL, NULL);
	CHECK(awdl_peer_get(state.peers.peers, &peer, NULL) == PEERS_MISSING);
	next = output;
	frame = buf_new_const(raw, sizeof raw);
	CHECK(awdl_rx_data(frame, &next, output + 1, &stranger, &self, &state) == RX_IGNORE_PEER);
	CHECK(next == output && awdl_peers_length(state.peers.peers) == 0);
	buf_free(frame);
	awdl_peers_free(state.peers.peers);
	return 0;
}
