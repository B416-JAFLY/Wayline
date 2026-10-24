/*
 * The wire protocol between the Rust firmware and host bridge.
 * Keep these definitions aligned with crates/wayline-proto/src/lib.rs.
 *
 * Every message is  type:u8 body... crc16:u16le , COBS-encoded and ended with
 * a 0x00. CRC-16/CCITT-FALSE over type and body. Integers are little-endian.
 * The ROM bootloader prints text on the same UART at boot; the delimiter and
 * CRC are how the host skips it.
 */
#pragma once

#define PROTO_VERSION 2
#define PROTO_MAX_FRAME 1600 /* largest 802.11 frame either way */

/* Host to radio. Each is answered by MSG_ACK unless noted. */
enum {
	MSG_HELLO = 0x01,       /* -> MSG_HELLO_RESP */
	MSG_SET_CHANNEL = 0x02, /* ch:u8 */
	MSG_SET_MAC = 0x03,     /* mac:6 */
	MSG_SET_FILTER = 0x04,  /* mode:u8 (FILTER_*) */
	MSG_TX = 0x05,          /* rate:u8 (RATE_*) count:u8 gap_ms:u16 frame... (no FCS) */
	MSG_TIME = 0x06,        /* -> MSG_TIME_RESP */
	MSG_STATS = 0x07,       /* -> MSG_STATS_RESP; optional "BA" aggregation / "UP" uplink stats */
	MSG_TRANSFER = 0x08,    /* optional UI extension: transfer header + label coverage */
	MSG_TRANSFER_STATUS = 0x09, /* direction:u8 -> MSG_TRANSFER_RESP */
};

/* Radio to host. */
enum {
	MSG_ACK = 0x80,        /* req:u8 err:i32 (esp_err_t) */
	MSG_HELLO_RESP = 0x81, /* proto:u8 mac:6 target:char[16] idf:char[32] */
	MSG_TX_DONE = 0x86,    /* acked:u8 dst:6, per frame sent, when the IDF reports it */
	MSG_TIME_RESP = 0x87,  /* us:u64 (esp_timer) */
	MSG_STATS_RESP = 0x88, /* rx rx_drop tx_ok tx_err tx_acked tx_noack bad_msg: u32 each */
	MSG_TRANSFER_RESP = 0x89, /* 32-byte current transfer header */
	MSG_RX = 0x90,         /* ts:u32 rssi:i8 ch:u8 rate:u8 sig_mode:u8 mcs:u8 cwb:u8 frame... (no FCS) */
	MSG_LOG = 0x91,        /* text */
};

/* MSG_STATS body "BA": optional 56-byte versioned reply, fourteen u32le fields:
 * version(1), enabled, active, tid(255 when inactive), window(0 when inactive),
 * requests, accepted, declined, response_errors, ended, ampdu_mpdu,
 * ampdu_rx_drop, control_drop, tx_report_drop. Old firmware may reply with the
 * ordinary 28-byte statistics instead; check length and version before use.
 * accepted counts enqueued successful ADDBA responses, not observed hardware BA.
 * ampdu_mpdu counts driver-marked aggregated data MPDUs addressed to this radio
 * and admitted by its filter.
 * Ordinary MSG_STATS and every other protocol-v2 message keep their layouts. */

/* MSG_STATS body "UP": optional 56-byte v1 uplink reply, fourteen u32le fields:
 * version(1), frames, batches, bytes, encode_us, write_us, max_write_us,
 * lock_wait_us, errors, max_batch_frames, max_batch_bytes, heap_free, heap_min,
 * link_core. Counters/timing totals wrap at u32; bytes counts driver-accepted
 * bytes, not end-to-end file delivery. Buffers remain bounded (4096 bytes/8 frames).
 * heap figures are internal byte-addressable memory; core is the installed link
 * interrupt's CPU. Old firmware may return 28-byte ordinary stats instead. */

/* MSG_TX's rate, the same on every chip (wifi_phy_rate_t isn't: its MCS
 * short-GI values move on chips with 802.11ax). Legacy rates are in radiotap's
 * 500 kbit/s units (2 = 1 Mbit/s, 12 = 6, 108 = 54); HT is RATE_MCS. */
#define RATE_MCS 0x80
#define RATE_MCS_SGI 0x10 /* with RATE_MCS; the low nibble is the MCS index */

enum {
	FILTER_AWDL = 0, /* AWDL action frames, and data frames on AWDL's BSSID or to us */
	FILTER_MGMT_DATA = 1,
	FILTER_ALL = 2, /* control frames too */
};
