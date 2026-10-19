# OWL patches for Windows/ESP32

Apply `owl-*.patch` in filename order to `OWL_COMMIT` in `native/owl/pins`.
`scripts/build-owl.ps1` reconstructs the patched source in `.cache/upstream/owl`.
Maintain changes in the patch files so builds reproduce them.

The patchset is based on
[t4t5/omdrop-owl 0.3.0, commit 8213aee](https://github.com/t4t5/omdrop-owl/tree/8213aee9f018ffd78a3fc71863a364fcddd938cd),
recorded as `OMDROP_OWL_PATCHSET_COMMIT`. The OWL base is
`832d70f815c3d4a06a02117bf0fc5e868daa1ff0`.

| Patches | Purpose |
| --- | --- |
| 01–06 | Upstream ESP32 radio fixes |
| 07-rx-host-tsft | USB receive timestamps (`-T`) |
| 08-unicast-max-wait | Two-second unicast queue deadline |
| 09-transfer-reliability | Peer refresh and retries under USB backpressure; adapted from upstream 07-intel-reliability |
| 10-amsdu-bound | Bounded receive output and partial-frame cleanup; adapted from upstream 08-amsdu-bound |
| 11-action-zero-padding | Zero-padded MIF trailers; adapted from upstream 09-action-zero-padding |
| 12-rx-flags-init | Radiotap Flags/RSSI initialization; adapted from upstream 11-rx-flags-init |
| 13-sync-channel-encoding | Correct master-channel decoding and fixed-radio channel advertisement |

Patch 13 is adapted from [PR #64](https://github.com/t4t5/omdrop-owl/pull/64),
head `4e0b1c1`. It decodes the stored master channel as OPCLASS independently of
the peer's channel-sequence encoding. Fixed radios advertise their configured channel;
hopping radios use their current channel and encoding.

The peer timeout is five seconds; received data refreshes active peers. Unicast retries
respect peer channels and the queue deadline. The USB backend reports queue-full events
separately from transmission errors. OWL does not accept ADDBA on behalf of the USB radio.
The experimental firmware handles receive agreements and hardware Block Ack locally; see
[receive aggregation](../../../docs/DEVELOPMENT.md#experimental-receive-aggregation).

## Verification

From the repository root with MinGW-w64 GCC and Git on PATH, and CMake/Ninja installed:

```powershell
scripts/build-owl.ps1 -Test
```

The script locates CMake/Ninja and uses CTest from the selected CMake installation.
`just test-owl` invokes the same command. For a build without test targets, configure CMake
with `-DBUILD_TESTING=OFF` and omit `-Test` on subsequent builds.

Tests cover receive bounds and cleanup, MIF padding, radiotap metadata, timestamps,
peer lifetime, USB retry/channel/deadline behavior, transfer UI IPC and channel
advertisements. USB codec and batching tests verify CRC/COBS equivalence, FIFO order,
queue wraparound, bounded batches, partial/overlapped writes and failure wakeups.
`owl_test_radio_io.exe --bench` measures host codec throughput.

Tests run without a serial port or Wintun adapter. Device interoperability and
wireless throughput require hardware testing.

The `owl_amsdu.c`, `owl_padding.c` and `owl_rx_flags.c` tests are adapted from the
same upstream project's `tests/` directory. Tests and patches retain GPL-3.0-or-later
licensing.
