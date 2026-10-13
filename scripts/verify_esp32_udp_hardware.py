#!/usr/bin/env python3
"""Bounded real-device acceptance for RuView ADR-018 single-antenna CSI UDP.

The device must already stream to --bind:--port. This script never configures or
flashes it. It bridges binary packets to the ESP32 adapter's JSON UDP contract.
Host receive times are used; ADR-018's sequence number is NOT a timestamp.
Output contains private RF data: choose a directory outside your checkout.
This checks data plumbing, not sensing accuracy or direct JSON serial support.
"""
import argparse
import collections
import hashlib
import ipaddress
import json
from pathlib import Path
import socket
import struct
import subprocess
import time


def decode(data, timestamp_ns, node):
    if len(data) < 4 or struct.unpack_from("<I", data)[0] != 0xC5110001:
        return None
    if len(data) < 20:
        raise ValueError("truncated ADR-018 header")
    _, observed_node, antennas, count, frequency, sequence, rssi, _, _, flags = (
        struct.unpack_from("<IBBHIIbbBB", data)
    )
    if observed_node != node or antennas != 1 or not 1 <= count <= 4096:
        raise ValueError("unexpected node, antenna count or subcarrier count")
    if len(data) != 20 + 2 * count:
        raise ValueError("ADR-018 payload length mismatch")
    channel = 14 if frequency == 2484 else (frequency - 2407) // 5
    if not 1 <= channel <= 14 or frequency != (2484 if channel == 14 else 2407 + 5 * channel):
        raise ValueError("unsupported channel frequency")
    # Preserve the firmware's bytes. No first_word_invalid boolean is carried
    # by this protocol; do not infer one or strip bytes that firmware sanitized.
    return dict(type="raw_csi", ts_ns=timestamp_ns, channel=channel,
                bandwidth_mhz=40 if flags & 1 else 20, rssi=rssi,
                iq_hex=data[20:].hex()), sequence


def run(binary, args):
    result = subprocess.run([str(binary), *map(str, args)], capture_output=True,
                            text=True, timeout=30, check=True)
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--bind", type=ipaddress.IPv4Address, required=True)
    parser.add_argument("--device", type=ipaddress.IPv4Address, required=True)
    parser.add_argument("--port", type=int, default=5005)
    parser.add_argument("--node", type=int, required=True)
    parser.add_argument("--frames", type=int, default=128)
    parser.add_argument("--seconds", type=int, default=45)
    args = parser.parse_args()
    if not (1 <= args.port <= 65535 and 0 <= args.node <= 255
            and 1 <= args.frames <= 10000 and 1 <= args.seconds <= 300):
        parser.error("invalid port, node, frame bound or duration")
    binary = args.binary.resolve(strict=True)
    output = args.output.resolve()
    repo = Path(__file__).resolve().parents[1]
    if output == repo or repo in output.parents:
        parser.error("private evidence directory must be outside this checkout")
    output.mkdir(parents=True, exist_ok=False)
    capture = output / "capture.rvcsi"
    records, packets = [], []
    ignored = 0
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver, \
            socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
        if hasattr(socket, "SO_EXCLUSIVEADDRUSE"):
            receiver.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        receiver.bind((str(args.bind), args.port))
        receiver.settimeout(0.5)
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
            probe.bind(("127.0.0.1", 0))
            relay_port = probe.getsockname()[1]
        proc = subprocess.Popen(
            [str(binary), "record", "--source", "esp32-udp", "--in",
             f"127.0.0.1:{relay_port}", "--out", str(capture), "--limit",
             str(args.frames), "--timeout-ms", str(args.seconds * 1000),
             "--source-id", "measured-esp32-c6-collector", "--session", "1"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            time.sleep(0.3)
            deadline = time.monotonic() + args.seconds
            while len(records) < args.frames and time.monotonic() < deadline:
                if proc.poll() is not None:
                    raise RuntimeError("recorder exited before capture completed")
                try:
                    data, peer = receiver.recvfrom(65535)
                except socket.timeout:
                    continue
                received_ns = time.time_ns()
                if peer[0] != str(args.device):
                    ignored += 1
                    continue
                decoded = decode(data, received_ns, args.node)
                if decoded is None:
                    ignored += 1
                    continue
                record, sequence = decoded
                sender.sendto(json.dumps(record).encode(), ("127.0.0.1", relay_port))
                records.append(record)
                packets.append(dict(received_ns=received_ns, sequence=sequence, hex=data.hex()))
            if len(records) != args.frames:
                raise RuntimeError(f"only {len(records)}/{args.frames} real CSI packets received")
            stdout, stderr = proc.communicate(timeout=5)
            if proc.returncode:
                raise RuntimeError(stderr)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.communicate(timeout=5)
    for name, rows in (("raw-packets.jsonl", packets), ("collector.jsonl", records)):
        (output / name).write_text("".join(json.dumps(row) + "\n" for row in rows))
    frames = [json.loads(line) for line in run(binary, ["replay", capture, "--json"]).splitlines()]
    if len(frames) != len(records):
        raise AssertionError("frame count mismatch")
    for frame, record in zip(frames, records):
        raw = bytes.fromhex(record["iq_hex"])
        signed = [value if value < 128 else value - 256 for value in raw]
        assert frame["timestamp_ns"] == record["ts_ns"]
        assert frame["i_values"] == signed[1::2] and frame["q_values"] == signed[::2]
        assert frame["channel"] == record["channel"]
        assert frame["bandwidth_mhz"] == record["bandwidth_mhz"]
        assert frame["rssi_dbm"] == record["rssi"]
        assert frame["adapter_kind"] == "Esp32"
        assert frame["validation"] in ("Accepted", "Degraded", "Recovered")
    offline = output / "offline.rvcsi"
    run(binary, ["record", "--source", "esp32-jsonl", "--in", output / "collector.jsonl",
                 "--out", offline, "--source-id", "measured-esp32-c6-collector", "--session", "1"])
    assert run(binary, ["replay", offline, "--json"]) == run(binary, ["replay", capture, "--json"])
    summary = json.loads(run(binary, ["inspect", capture, "--json"]))
    events = json.loads(run(binary, ["events", capture, "--json"]))
    run(binary, ["calibrate", "--in", capture, "--out", output / "baseline.json"])
    run(binary, ["export", "ruvector", "--in", capture, "--out", output / "memory.jsonl"])
    baseline = json.loads((output / "baseline.json").read_text())
    memory = [json.loads(line) for line in (output / "memory.jsonl").read_text().splitlines()]
    assert baseline and memory
    report = dict(result="PASS", scope="MEASURED binary UDP -> collector -> JSON UDP and file",
                  cli_version=run(binary, ["--version"]).strip(), frames=len(frames),
                  ignored_datagrams=ignored, timestamp_source="host receive time_ns",
                  validation=dict(collections.Counter(f["validation"] for f in frames)),
                  subcarrier_counts=dict(collections.Counter(f["subcarrier_count"] for f in frames)),
                  events=len(events), memory_records=len(memory), summary=summary,
                  checks=["exact signed I/Q", "exact host timestamps", "metadata", "validation",
                          "offline/live replay identity", "events", "calibration", "RF memory"],
                  sha256={p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                          for p in output.iterdir() if p.is_file()})
    (output / "verification.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
