#!/usr/bin/env python3
"""Linux/macOS transport acceptance using local UDP and a serial pseudo-terminal.
Run after cargo build -p rvcsi-cli. This is not physical hardware qualification.
"""
import json
import os
from pathlib import Path
import pty
import socket
import subprocess
import tempfile
import time
import threading

BINARY = Path(__file__).resolve().parents[1] / "target/debug/rvcsi"
RECORD = (json.dumps(dict(type="raw_csi", ts_ns=123, channel=6, rssi=-55,
                         iq_hex="0102" * 64)) + "\n").encode()


def check(transport, endpoint, send):
    with tempfile.TemporaryDirectory() as tmp:
        output = Path(tmp) / "capture.rvcsi"
        proc = subprocess.Popen([str(BINARY), "record", "--source", transport,
                                 "--in", endpoint, "--out", str(output),
                                 "--limit", "3", "--timeout-ms", "2000"],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        stopped = threading.Event()
        def pump():
            while not stopped.wait(0.05):
                record = json.loads(RECORD)
                record["ts_ns"] = time.monotonic_ns()
                send((json.dumps(record) + "\n").encode())
        thread = threading.Thread(target=pump, daemon=True)
        thread.start()
        try:
            stdout, stderr = proc.communicate(timeout=10)
            assert proc.returncode == 0, (stdout, stderr)
            frames = [json.loads(line) for line in output.read_text().splitlines()[1:]]
            assert len(frames) == 3
            assert all(f["validation"] == "Accepted" and f["i_values"] == [2.0]*64 for f in frames)
            print(transport + ": 3 validated frames, capture published")
        finally:
            stopped.set()
            thread.join()
            if proc.poll() is None:
                proc.kill()
                proc.wait()


if __name__ == "__main__":
    master, slave = pty.openpty()
    try:
        check("esp32-serial", os.ttyname(slave), lambda data: os.write(master, data))
    finally:
        os.close(master)
        os.close(slave)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
        check("esp32-udp", f"127.0.0.1:{port}", lambda data: sender.sendto(data, ("127.0.0.1", port)))
