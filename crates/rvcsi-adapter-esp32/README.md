# rvcsi-adapter-esp32

Native Rust `CsiSource` for ESP32 collector JSONL files, serial ports and UDP.
Uses the existing collector wire format, not arbitrary ESP-IDF CSV or binary firmware protocols.
No firmware flashing or radio configuration is performed.

```json
{"type":"raw_csi","ts_ns":1000000000,"channel":6,"bandwidth_mhz":20,"rssi":-55,"iq_hex":"ff02807f","first_word_invalid":false}
```

`ts_ns` is required and preserved in nanoseconds; the producer owns timestamp epoch and rollover conversion. `iq_hex` contains signed int8 bytes in imaginary/real order. Bandwidth defaults to 20 MHz. RSSI outside -110..0 dBm is treated as unavailable, matching the legacy collector bridge. `first_word_invalid` removes the first four bytes when true. The adapter does not guess chip identity or fixed subcarrier count.

Each line/datagram must be a CSI record: no boot logs, CSV, blank lines or status messages. Maximum encoded record is 32 KiB; maximum raw input is 4096 I/Q pairs. UDP uses a bounded 65,535-byte receive buffer so oversized datagrams are rejected and counted consistently on every platform before JSON parsing. Invalid hex, incomplete pairs, missing fields, invalid channels and invalid bandwidths fail explicitly. Returned frames are Pending; `CaptureRuntime` and the CLI validate before exposure.

```bash
# Build from this checkout; registry release is a separate step.
cargo build --release -p rvcsi-cli
./target/release/rvcsi record --source esp32-jsonl --in capture.csi.jsonl --out capture.rvcsi
./target/release/rvcsi record --source esp32-serial --in /dev/ttyUSB0 --baud 115200 --limit 1000 --timeout-ms 5000 --out serial.rvcsi
./target/release/rvcsi record --source esp32-udp --in 127.0.0.1:5501 --limit 1000 --timeout-ms 5000 --out udp.rvcsi
./target/release/rvcsi inspect capture.rvcsi --json
./target/release/rvcsi events capture.rvcsi --json
./target/release/rvcsi calibrate --in capture.rvcsi --out baseline.json
./target/release/rvcsi export ruvector --in capture.rvcsi --out memory.jsonl
```

Use a specific LAN bind address for a remote ESP32; loopback accepts local senders only. UDP has no authentication, ordering or delivery guarantees. Restrict senders using host/network firewall rules. One JSON record per UDP datagram. Serial is configured as 8N1 with no flow control by the serialport defaults; use firmware matching the requested baud rate.

Live CLI capture requires a positive frame limit. Receive timeout, parse error or validation error fails the capture without publishing the output. Existing output files are never overwritten. The Rust adapter allows continuing after a malformed bounded record; an oversized stream or I/O error closes the source. `stop()` releases the port/socket. File input is streaming and does not load the entire capture.

Hardware qualification remains separate from file, loopback and pseudo-terminal tests. Firmware or a collector must emit this JSON contract to use the adapter.

The [2026-10-12 ESP32-C6 verification](../../docs/verification/esp32-c6-2026-10-12.md) exercised real RuView binary UDP through a bounded collector, then this adapter's JSON UDP and file inputs. It does not qualify direct JSON serial or other firmware/boards. The accompanying script is a hardware acceptance reproducer, not a general firmware protocol adapter.
