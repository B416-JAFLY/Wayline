/* Verify synchronization advertisements after adopting a peer's encoding.
 * Adapted from the channel correction in t4t5/omdrop-owl#64. */
#include <stdio.h>
#include "state.h"
#include "tx.h"
#include "wire.h"

int main(void) {
	const struct ether_addr self = {{2, 0, 0, 0, 0, 1}};
	const struct awdl_chan masters[] = {CHAN_OPCLASS_6, CHAN_OPCLASS_44, CHAN_OPCLASS_149};
	const enum awdl_chan_encoding encodings[] = {AWDL_CHAN_ENC_SIMPLE, AWDL_CHAN_ENC_LEGACY, AWDL_CHAN_ENC_OPCLASS};
	int failed = 0;
	for (unsigned m = 0; m < sizeof masters / sizeof masters[0]; m++) {
		for (unsigned e = 0; e < sizeof encodings / sizeof encodings[0]; e++) {
			for (int fixed = 0; fixed <= 1; fixed++) {
				struct awdl_state state = {0};
				uint8_t bytes[512] = {0};
				awdl_init_state(&state, "test", &self, masters[m], clock_time_us());
				state.channel.enc = encodings[e];
				state.channel.fixed_radio = fixed;
				state.channel.current = CHAN_OPCLASS_44;
				if (encodings[e] == AWDL_CHAN_ENC_LEGACY) {
					state.channel.current.legacy.flags = 0;
					state.channel.current.legacy.chan_num = 44;
				}
				awdl_chanseq_init_static(state.channel.sequence, &state.channel.current);
				awdl_init_sync_params_tlv(bytes, &state);
				const struct awdl_sync_params_tlv *tlv = (const void *) bytes;
				int master = awdl_chan_num(masters[m], AWDL_CHAN_ENC_OPCLASS);
				if (tlv->master_channel != master || tlv->next_aw_channel != (fixed ? master : 44)) {
					fprintf(stderr, "master=%d encoding=%d fixed=%d: advertised master=%u next=%u\n",
					        master, encodings[e], fixed, tlv->master_channel, tlv->next_aw_channel);
					failed = 1;
				}
				awdl_peers_free(state.peers.peers);
			}
		}
	}
	return failed;
}
