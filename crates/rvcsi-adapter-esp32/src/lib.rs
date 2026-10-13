//! ESP32 collector JSONL over files, serial and UDP. Frames remain Pending
//! until the runtime or recorder validates them. No firmware is flashed.
#![forbid(unsafe_code)]

use rvcsi_core::{
    AdapterKind, AdapterProfile, CsiFrame, CsiSource, FrameId, Result, RvcsiError, SessionId,
    SourceHealth, SourceId,
};
use serde::Deserialize;
use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    net::{SocketAddr, UdpSocket},
    path::Path,
    time::Duration,
};

/// Maximum encoded record size. Bounds allocations before JSON parsing.
pub const MAX_RECORD_BYTES: usize = 32768;
// Receive a whole UDP datagram before applying the record bound. Winsock
// reports WSAEMSGSIZE instead of returning a truncated oversized datagram.
const MAX_UDP_DATAGRAM_BYTES: usize = 65535;
const MAX_SUBCARRIERS: usize = 4096;

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    ts_ns: u64,
    channel: u16,
    #[serde(default = "default_bandwidth")]
    bandwidth_mhz: u16,
    rssi: Option<i16>,
    iq_hex: String,
    #[serde(default)]
    first_word_invalid: bool,
}
fn default_bandwidth() -> u16 {
    20
}

/// Decode the collector's `raw_csi` JSON object, preserving nanosecond timestamps.
/// I/Q bytes are signed int8 in ESP-IDF imaginary/real order. When firmware
/// flags first_word_invalid, remove the first four bytes (two pairs).
pub fn decode_record(
    bytes: &[u8],
    id: FrameId,
    session: SessionId,
    source: SourceId,
) -> Result<CsiFrame> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(RvcsiError::parse(0, "record exceeds 32768 bytes"));
    }
    let r: Record =
        serde_json::from_slice(bytes).map_err(|e| RvcsiError::parse(0, e.to_string()))?;
    if r.kind != "raw_csi" {
        return Err(RvcsiError::parse(0, "expected type raw_csi"));
    }
    if !(1..=14).contains(&r.channel) || ![20, 40].contains(&r.bandwidth_mhz) {
        return Err(RvcsiError::parse(
            0,
            "expected 2.4 GHz channel 1..14 and bandwidth 20 or 40",
        ));
    }
    let h = r.iq_hex.as_bytes();
    if h.is_empty() || h.len() % 4 != 0 || h.len() > MAX_SUBCARRIERS * 4 {
        return Err(RvcsiError::parse(
            0,
            "iq_hex must contain 1..4096 complete I/Q pairs",
        ));
    }
    let nibble = |b: u8| -> Result<u8> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(RvcsiError::parse(0, "invalid hex digit")),
        }
    };
    let mut raw = Vec::with_capacity(h.len() / 2);
    for b in h.chunks_exact(2) {
        raw.push((nibble(b[0])? * 16 + nibble(b[1])?) as i8);
    }
    let skip = if r.first_word_invalid { 4 } else { 0 };
    if raw.len() <= skip {
        return Err(RvcsiError::parse(
            0,
            "no valid pairs after first_word_invalid",
        ));
    }
    let (mut i, mut q) = (Vec::new(), Vec::new());
    for pair in raw[skip..].chunks_exact(2) {
        q.push(pair[0] as f32);
        i.push(pair[1] as f32);
    }
    let mut f = CsiFrame::from_iq(
        id,
        session,
        source,
        AdapterKind::Esp32,
        r.ts_ns,
        r.channel,
        r.bandwidth_mhz,
        i,
        q,
    );
    // Legacy collectors use sentinel RSSI values: retain plausible values only.
    f.rssi_dbm = r.rssi.filter(|r| (-110..=0).contains(r));
    Ok(f)
}

enum Input {
    Reader(Box<dyn BufRead + Send>),
    Udp(UdpSocket),
}
/// Pull-based ESP32 source. Parse errors consume exactly one record; callers may
/// continue. Oversized stream lines close the source to avoid unbounded draining.
pub struct Esp32Adapter {
    input: Option<Input>,
    profile: AdapterProfile,
    source: SourceId,
    session: SessionId,
    health: SourceHealth,
    record: usize,
}
impl Esp32Adapter {
    fn new(input: Input, source: SourceId, session: SessionId, live: bool) -> Self {
        let mut profile = AdapterProfile::offline(AdapterKind::Esp32);
        profile.supports_live_capture = live;
        profile.supported_channels = (1..=14).collect();
        profile.supported_bandwidths_mhz = vec![20, 40];
        Self {
            input: Some(input),
            profile,
            source,
            session,
            health: SourceHealth::fresh(true),
            record: 0,
        }
    }
    /// Read collector JSONL from a file, incrementally.
    pub fn open(path: impl AsRef<Path>, source: SourceId, session: SessionId) -> Result<Self> {
        Ok(Self::from_reader(
            BufReader::new(File::open(path)?),
            source,
            session,
        ))
    }
    /// Read JSONL from an already configured stream (including serial bridges).
    pub fn from_reader(
        reader: impl BufRead + Send + 'static,
        source: SourceId,
        session: SessionId,
    ) -> Self {
        Self::new(Input::Reader(Box::new(reader)), source, session, false)
    }
    /// Open and configure a native serial device. Timeout is an error, not EOF.
    pub fn serial(
        path: &str,
        baud: u32,
        timeout: Duration,
        source: SourceId,
        session: SessionId,
    ) -> Result<Self> {
        if baud == 0 || timeout.is_zero() {
            return Err(RvcsiError::Config(
                "baud and timeout must be positive".into(),
            ));
        }
        let port = serialport::new(path, baud)
            .timeout(timeout)
            .open()
            .map_err(|e| RvcsiError::adapter("esp32", e.to_string()))?;
        Ok(Self::new(
            Input::Reader(Box::new(BufReader::new(port))),
            source,
            session,
            true,
        ))
    }
    /// Bind a UDP receiver. Exactly one JSON record per datagram; the caller
    /// chooses the interface explicitly. This transport is unauthenticated.
    pub fn bind(
        address: SocketAddr,
        timeout: Duration,
        source: SourceId,
        session: SessionId,
    ) -> Result<Self> {
        if timeout.is_zero() {
            return Err(RvcsiError::Config("timeout must be positive".into()));
        }
        let socket = UdpSocket::bind(address)?;
        socket.set_read_timeout(Some(timeout))?;
        Ok(Self::new(Input::Udp(socket), source, session, true))
    }
    /// Bound UDP address, useful when port zero was requested.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        match &self.input {
            Some(Input::Udp(s)) => Ok(s.local_addr()?),
            _ => Err(RvcsiError::Config("not a UDP source".into())),
        }
    }
}
impl CsiSource for Esp32Adapter {
    fn profile(&self) -> &AdapterProfile {
        &self.profile
    }
    fn source_id(&self) -> &SourceId {
        &self.source
    }
    fn session_id(&self) -> SessionId {
        self.session
    }
    fn health(&self) -> SourceHealth {
        self.health.clone()
    }
    fn stop(&mut self) -> Result<()> {
        self.input = None;
        self.health.connected = false;
        Ok(())
    }
    fn next_frame(&mut self) -> Result<Option<CsiFrame>> {
        let Some(input) = &mut self.input else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        let read = match input {
            Input::Reader(r) => r
                .take((MAX_RECORD_BYTES + 1) as u64)
                .read_until(b'\n', &mut bytes),
            Input::Udp(s) => {
                bytes.resize(MAX_UDP_DATAGRAM_BYTES, 0);
                s.recv(&mut bytes).inspect(|&n| {
                    bytes.truncate(n);
                })
            }
        };
        let n = match read {
            Ok(n) => n,
            Err(e) => {
                self.health.status = Some(e.to_string());
                self.stop()?;
                return Err(e.into());
            }
        };
        if n == 0 && matches!(input, Input::Reader(_)) {
            self.stop()?;
            return Ok(None);
        }
        self.record += 1;
        let decoded = if n > MAX_RECORD_BYTES {
            self.stop()?;
            Err(RvcsiError::parse(
                self.record,
                "record size limit exceeded; source closed",
            ))
        } else {
            decode_record(
                &bytes,
                FrameId(self.health.frames_delivered),
                self.session,
                self.source.clone(),
            )
        };
        match decoded {
            Ok(frame) => {
                self.health.frames_delivered += 1;
                Ok(Some(frame))
            }
            Err(e) => {
                self.health.frames_rejected += 1;
                self.health.status = Some(e.to_string());
                Err(RvcsiError::parse(self.record, e.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(hex: &str) -> Vec<u8> {
        format!(r#"{{"type":"raw_csi","ts_ns":123,"channel":6,"rssi":-55,"iq_hex":"{hex}"}}"#)
            .into_bytes()
    }
    fn decode(b: &[u8]) -> Result<CsiFrame> {
        decode_record(b, FrameId(0), SessionId(2), SourceId::from("test"))
    }
    #[test]
    fn signed_imaginary_real_order() {
        let f = decode(&sample("ff02807f")).unwrap();
        assert_eq!(f.i_values, vec![2., 127.]);
        assert_eq!(f.q_values, vec![-1., -128.]);
        assert_eq!(f.timestamp_ns, 123);
        assert_eq!(f.rssi_dbm, Some(-55));
        assert_eq!(f.validation, rvcsi_core::ValidationStatus::Pending);
    }
    #[test]
    fn rejects_bad_payloads() {
        for h in ["", "0", "001", "00", "000000", "zzzz", "éé"] {
            assert!(decode(&sample(h)).is_err(), "{h}");
        }
        assert!(decode(&vec![b' '; MAX_RECORD_BYTES + 1]).is_err());
        assert!(decode(br#"{"type":"raw_csi"}"#).is_err());
        assert!(decode(&sample("0102").iter().copied().take(10).collect::<Vec<_>>()).is_err());
    }
    #[test]
    fn invalid_first_word_and_rssi_sentinel() {
        let b = String::from_utf8(sample("010203040506"))
            .unwrap()
            .replace("\"rssi\":-55", "\"rssi\":64,\"first_word_invalid\":true");
        let f = decode(b.as_bytes()).unwrap();
        assert_eq!(f.i_values, vec![6.]);
        assert_eq!(f.q_values, vec![5.]);
        assert_eq!(f.rssi_dbm, None);
    }
    #[test]
    fn reader_recovery_eof_and_stop() {
        let mut data = b"bad\n".to_vec();
        data.extend(sample("0102"));
        let mut a = Esp32Adapter::from_reader(
            std::io::Cursor::new(data),
            SourceId::from("t"),
            SessionId(0),
        );
        assert!(a.next_frame().is_err());
        assert!(a.next_frame().unwrap().is_some());
        assert!(a.next_frame().unwrap().is_none());
        assert_eq!(a.health().frames_rejected, 1);
        assert_eq!(a.health().frames_delivered, 1);
        assert!(!a.health().connected);
        a.stop().unwrap();
        assert!(a.next_frame().unwrap().is_none());
    }
    #[test]
    fn oversized_stream_closes_without_draining() {
        let mut a = Esp32Adapter::from_reader(
            std::io::Cursor::new(vec![b'x'; MAX_RECORD_BYTES * 2]),
            SourceId::from("t"),
            SessionId(0),
        );
        assert!(a.next_frame().is_err());
        assert!(!a.health().connected);
        assert!(a.next_frame().unwrap().is_none());
    }
    #[test]
    fn udp_loopback_and_timeout() {
        let mut a = Esp32Adapter::bind(
            "127.0.0.1:0".parse().unwrap(),
            Duration::from_millis(30),
            SourceId::from("udp"),
            SessionId(1),
        )
        .unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        sender
            .send_to(&sample("0102"), a.local_addr().unwrap())
            .unwrap();
        assert!(a.next_frame().unwrap().is_some());
        assert!(a.next_frame().is_err());
        assert!(!a.health().connected);
    }
    #[test]
    fn udp_oversize_is_not_accepted_as_truncated_json() {
        let mut a = Esp32Adapter::bind(
            "127.0.0.1:0".parse().unwrap(),
            Duration::from_secs(1),
            SourceId::from("udp"),
            SessionId(1),
        )
        .unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        // Darwin's default UDP send buffer cannot hold this test datagram.
        // Size this socket explicitly so the adapter's rejection is exercised.
        socket2::SockRef::from(&sender)
            .set_send_buffer_size(MAX_UDP_DATAGRAM_BYTES)
            .unwrap();
        sender
            .send_to(&vec![b' '; MAX_RECORD_BYTES + 100], a.local_addr().unwrap())
            .unwrap();
        assert!(a.next_frame().is_err());
        assert_eq!(a.health().frames_rejected, 1);
        assert!(!a.health().connected);
        assert!(a.next_frame().unwrap().is_none());
    }
}
