//! `relay` CLI 冒烟测试。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn write_config(dir: &Path) -> PathBuf {
    let path = dir.join("relay.toml");
    std::fs::write(
        &path,
        format!(
            "listen = \"127.0.0.1:0\"\ndata = \"{}\"\nmax_payload_bytes = 1024\nchannel_quota_bytes = 4096\n",
            dir.join("relay.db").display()
        ),
    )
    .unwrap();
    path
}

fn run(config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_relay"))
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .unwrap()
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_manages_channel_device_member_and_token() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path());
    let channel = "c".repeat(32);
    let device = "d".repeat(32);

    let output = run(&config, &["channel", "new", "--id", &channel]);
    assert_ok(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains(&channel));

    let output = run(&config, &["device", "new", "--device-id", &device]);
    assert_ok(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&device));
    assert!(stdout.contains("token:"));

    let output = run(
        &config,
        &["member", "add", "--channel", &channel, "--device", &device],
    );
    assert_ok(&output);

    let output = run(&config, &["channel", "list"]);
    assert_ok(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains(&channel));

    let output = run(&config, &["stats", "--channel", &channel]);
    assert_ok(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&channel));
    assert!(stdout.contains("next_seq=1"));
    assert!(stdout.contains(&format!("device={device}")));
    assert!(stdout.contains("acked_cursor=0"));

    let output = run(&config, &["token", "rotate", "--device", &device]);
    assert_ok(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("token:"));

    let output = run(
        &config,
        &[
            "member",
            "remove",
            "--channel",
            &channel,
            "--device",
            &device,
        ],
    );
    assert_ok(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("removed=true"));

    let output = run(&config, &["device", "revoke", "--device", &device]);
    assert_ok(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("revoked=true"));
}
