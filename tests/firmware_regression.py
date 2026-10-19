#!/usr/bin/env python3
"""Protocol-v2 regression against a real radio. Stop OWL before opening its port.

Does not send files. --tx-frames optionally emits broadcast test frames, like radio_probe.py link.
"""
import argparse
import json
import statistics
import struct
import time

import serial
from radio_probe import AWDL_BSSID, STATS_FIELDS, cobs_decode, cobs_encode, crc16, mac_str


class Link:
    def __init__(self, port):
        self.port = serial.Serial()
        self.port.port, self.port.baudrate, self.port.timeout = port, 3_000_000, 0.05
        self.port.dtr = self.port.rts = False
        self.port.open()
        self.pending = bytearray()
        self.rx = self.tx_done = self.bad = 0

    @staticmethod
    def encode(kind, body=b""):
        message = bytes([kind]) + body
        return cobs_encode(message + struct.pack("<H", crc16(message)))

    def wait(self, expected, request, timeout=5):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            delimiter = self.pending.find(0)
            if delimiter < 0:
                self.pending.extend(self.port.read(self.port.in_waiting or 1))
                continue
            encoded = bytes(self.pending[:delimiter])
            del self.pending[:delimiter + 1]
            if not encoded:
                continue
            try:
                raw = cobs_decode(encoded)
                if len(raw) < 3 or crc16(raw[:-2]) != int.from_bytes(raw[-2:], "little"):
                    raise ValueError("CRC")
            except ValueError:
                self.bad += 1
                continue
            kind, body = raw[0], raw[1:-2]
            if kind == 0x90:
                assert 10 <= len(body) <= 1610, "invalid RX metadata/frame length"
                self.rx += 1
            elif kind == 0x86:
                assert len(body) == 7, "invalid TX_DONE layout"
                self.tx_done += 1
            if kind == expected:
                if kind == 0x80:
                    assert len(body) == 5 and body[0] == request, "invalid ACK layout"
                    return struct.unpack("<i", body[1:])[0]
                return body
        raise TimeoutError(f"No {expected:#x} reply to {request:#x}")

    def request(self, kind, body=b"", expected=0x80):
        self.port.write(self.encode(kind, body))
        return self.wait(expected, kind)

    def stats(self):
        return dict(zip(STATS_FIELDS, struct.unpack("<7I", self.request(7, expected=0x88))))

    def ui_status(self):
        fields = ["uptime_ms", "stage", "ticks", "frames", "keys", "hardware"]
        return dict(zip(fields, struct.unpack("<6I", self.request(7, b"UI", expected=0x88))))


def run(link, rounds, tx_frames, soak_seconds):
    hello = link.request(1, expected=0x81)
    assert len(hello) == 55 and hello[0] == 2
    assert hello[7:23].rstrip(b"\0") == b"esp32s3"
    print(f"HELLO: proto 2, {mac_str(hello[1:7])}, {hello[23:].rstrip(bytes([0])).decode()}")
    assert link.request(3, hello[1:7]) == 0  # Wi-Fi stop/start + MAC reapply path.
    assert link.request(1, expected=0x81) == hello
    for channel in [1, 11, 6]:
        assert link.request(2, bytes([channel])) == 0
    for mode in [1, 2, 0]:
        assert link.request(4, bytes([mode])) == 0

    invalid = [(2, b"", 0x104), (2, b"\0", 0x102), (2, b"\x0f", 0x102),
               (3, bytes(5), 0x104), (4, b"", 0x104), (4, b"\x03", 0x102),
               (5, bytes(27), 0x104), (5, bytes([3]) + bytes(27), 0x102),
               (0xfe, b"", 0x106)]
    for kind, body, error in invalid:
        assert link.request(kind, body) == error, f"wrong error for {kind:#x}"
    print(f"Commands: MAC/channel/filter and {len(invalid)} error cases passed")

    before = link.stats()["bad_msg"]
    broken_crc = bytes([1]) + struct.pack("<H", crc16(bytes([1])) ^ 1)
    link.port.write(cobs_encode(broken_crc) + b"\xff\x01\0" + b"\x01" * 1800 + b"\0\0\0")
    after = link.stats()["bad_msg"]
    assert (after - before) & 0xffffffff == 3, (before, after)
    for byte in link.encode(1):
        link.port.write(bytes([byte]))
        time.sleep(0.002)
    assert link.wait(0x81, 1) == hello
    print("Framing: CRC error, malformed COBS, overflow, empty delimiters and split HELLO passed")

    rtts, previous = [], 0
    for _ in range(rounds):
        start = time.perf_counter()
        current, = struct.unpack("<Q", link.request(6, expected=0x87))
        assert current >= previous, "timer moved backwards / firmware reset"
        previous = current
        rtts.append((time.perf_counter() - start) * 1000)
    rtts.sort()
    print(f"TIME: {rounds} passed, median={statistics.median(rtts):.2f} ms, "
          f"p99={rtts[max(0, int(rounds * .99) - 1)]:.2f} ms")

    if tx_frames:
        before = link.stats()
        done_before = link.tx_done
        frame = struct.pack("<BBH6s6s6sH", 8, 0, 0, b"\xff" * 6, hello[1:7], AWDL_BSSID, 0) + bytes(1400)
        for _ in range(tx_frames):
            assert link.request(5, struct.pack("<BBH", 108, 1, 0) + frame) == 0
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            after = link.stats()
            if link.tx_done - done_before >= tx_frames:
                break
        assert (after["tx_ok"] - before["tx_ok"]) & 0xffffffff == tx_frames
        assert after["tx_err"] == before["tx_err"]
        assert link.tx_done - done_before == tx_frames
        print(f"TX: {tx_frames} x {len(frame)} bytes accepted, all TX_DONE callbacks received")
    if soak_seconds:
        deadline = time.monotonic() + soak_seconds
        samples = 0
        while time.monotonic() < deadline:
            current, = struct.unpack("<Q", link.request(6, expected=0x87))
            assert current >= previous, "timer moved backwards / firmware reset during soak"
            previous = current
            samples += 1
            time.sleep(0.2)
        assert link.request(1, expected=0x81) == hello
        print(f"Soak: {soak_seconds}s, {samples} monotonic TIME replies, HELLO unchanged")
    assert link.bad == 0, "corrupt frames received from firmware"
    print(json.dumps({"stats": link.stats(), "observed_rx": link.rx,
                      "observed_tx_done": link.tx_done, "host_bad_frames": link.bad}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("-p", "--port", default="COM4")
    parser.add_argument("-n", "--rounds", type=int, default=200)
    parser.add_argument("--tx-frames", type=int, default=0)
    parser.add_argument("--soak-seconds", type=int, default=0,
                        help="keep polling while UI/background tasks run")
    parser.add_argument("--check-ui", action="store_true",
                        help="verify Cardputer keyboard initialization and UI task liveness")
    args = parser.parse_args()
    if args.rounds < 1 or args.tx_frames < 0 or args.soak_seconds < 0:
        parser.error("rounds must be positive; tx-frames and soak-seconds must be nonnegative")
    link = Link(args.port)
    try:
        run(link, args.rounds, args.tx_frames, args.soak_seconds)
        if args.check_ui:
            before = link.ui_status()
            time.sleep(1)
            after = link.ui_status()
            assert after["hardware"] & 0x100, f"keyboard not initialized: {after}"
            assert after["ticks"] > before["ticks"], f"UI task stalled: {before} -> {after}"
            print("UI:", json.dumps(after))
    finally:
        # These are the service's normal settings, even after a failed assertion.
        try:
            link.request(2, bytes([6]))
            link.request(4, bytes([0]))
        finally:
            link.port.close()


if __name__ == "__main__":
    main()
