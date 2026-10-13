# ADR-097: native ESP32 collector source

Status: implemented. [ESP32-C6 collector-path hardware verification](../verification/esp32-c6-2026-10-12.md) passed; direct JSON serial and other boards remain unqualified.

## Contract and decision

Replace the Python-only bridge path with a safe Rust CsiSource supporting bounded JSONL files, native serial and one-record-per-datagram UDP. Reuse the existing raw_csi/ts_ns/channel/rssi/iq_hex contract. Maintain imaginary/real signed int8 ordering. Never infer timestamp units, chip identity or medical meaning. Parse to Pending CsiFrame; reuse core validation, runtime DSP and events.

Control flow: bounded read → strict decode → core validation → temporary recorder → flush and sync → publish without overwrite. Any capture error drops the temporary file. Empty capture fails. Live capture requires a frame limit and receive timeout. A finite file ends only on EOF; transport timeout is an error.

Serialport with default features disabled avoids a Linux libudev build dependency while retaining native serial setup. A generic BufRead constructor supports tests and existing bridges. Runtime exposes source composition and a JSONL convenience constructor. CLI output remains the standard .rvcsi format, so inspect/events/calibrate/export need no new data model.

## Trust boundary

File/serial/UDP data are untrusted. Limit each record to 32768 bytes before JSON parsing and raw data to 4096 pairs. Validate hex, pair alignment, unsigned timestamps, channel and bandwidth. Stop on oversized lines rather than draining attacker-controlled endless input. UDP is unauthenticated and cannot establish device identity; bind explicitly and firewall remote senders. No automatic remote discovery or firmware modification.

## Acceptance

Adapter tests cover signed I/Q, first-word invalidity, malformed records, size bounds, source counters, EOF/stop, UDP loopback and timeout. CLI integration covers record → inspect → replay → events → calibration → RF memory export, direct runtime events, malformed capture rollback and overwrite prevention. Serial transport must additionally pass a local pseudo-terminal smoke test; physical ESP32 accuracy and sustained capture require a device session.

Rollback: remove ESP32 dependency, workspace member and CLI dispatch. Existing Nexmon and .rvcsi schemas are unchanged. The Python bridge remains for compatibility, but new integrations should use the Rust path.
