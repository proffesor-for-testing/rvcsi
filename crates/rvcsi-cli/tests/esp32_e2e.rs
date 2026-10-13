use std::process::Command;
fn run(args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_rvcsi"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}
#[test]
fn esp32_record_inspect_replay_events_calibrate_export() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.jsonl");
    let cap = dir.path().join("capture.rvcsi");
    let baseline = dir.path().join("baseline.json");
    let memory = dir.path().join("memory.jsonl");
    let data=(0..64).map(|k|serde_json::json!({"type":"raw_csi","ts_ns":1000+k*50_000_000u64,"channel":6,"rssi":-55,"iq_hex":(0..64).map(|s| format!("01{:02x}", if k%16<8 {2} else {2+((s+k)%5)*20})).collect::<String>()}).to_string()).collect::<Vec<_>>().join("\n");
    std::fs::write(&input, data).unwrap();
    let i = input.to_str().unwrap();
    let c = cap.to_str().unwrap();
    run(&[
        "record",
        "--source",
        "esp32-jsonl",
        "--in",
        i,
        "--out",
        c,
        "--session",
        "42",
    ]);
    let frames = run(&["replay", c, "--json"]);
    assert_eq!(frames.lines().count(), 64);
    let first: serde_json::Value = serde_json::from_str(frames.lines().next().unwrap()).unwrap();
    assert_eq!(first["adapter_kind"], "Esp32");
    assert_eq!(first["session_id"], 42);
    assert_eq!(first["validation"], "Accepted");
    let summary = run(&["inspect", c, "--json"]);
    assert!(serde_json::from_str::<serde_json::Value>(&summary).is_ok());
    let events = run(&["events", c, "--json"]);
    let parsed: Vec<rvcsi_core::CsiEvent> = serde_json::from_str(&events).unwrap();
    assert!(!parsed.is_empty());
    for event in parsed {
        event.validate().unwrap();
    }
    run(&["calibrate", "--in", c, "--out", baseline.to_str().unwrap()]);
    assert!(baseline.metadata().unwrap().len() > 0);
    run(&[
        "export",
        "ruvector",
        "--in",
        c,
        "--out",
        memory.to_str().unwrap(),
    ]);
    assert!(memory.metadata().unwrap().len() > 0);
    let mut rt = rvcsi_runtime::CaptureRuntime::open_esp32_jsonl(i, "direct", 42).unwrap();
    let events = rt.drain_events().unwrap();
    assert!(!events.is_empty());
    for e in events {
        e.validate().unwrap();
    }
    let before = std::fs::read(&cap).unwrap();
    assert!(!Command::new(env!("CARGO_BIN_EXE_rvcsi"))
        .args(["record", "--source", "esp32-jsonl", "--in", i, "--out", c])
        .status()
        .unwrap()
        .success());
    assert_eq!(before, std::fs::read(&cap).unwrap());
}
#[test]
fn malformed_input_does_not_publish_partial_capture() {
    let dir = tempfile::tempdir().unwrap();
    let i = dir.path().join("bad.jsonl");
    let o = dir.path().join("out.rvcsi");
    std::fs::write(&i, "{bad}\n").unwrap();
    assert!(!Command::new(env!("CARGO_BIN_EXE_rvcsi"))
        .args([
            "record",
            "--source",
            "esp32-jsonl",
            "--in",
            i.to_str().unwrap(),
            "--out",
            o.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    assert!(!o.exists());
}
