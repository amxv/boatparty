//! A disposable unprivileged OpenSSH server and real loopback forwards.
#![cfg(target_os = "linux")]
mod common;
#[path = "common/forward_death.rs"]
mod forward_death;
use common::e2e::E2e;
use forward_death::{Process, assert_released};
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires a local OpenSSH server, exercised by Linux CI and just remote-check"]
fn killing_the_owner_closes_active_ssh_forwards() {
    let setup = E2e::new("forward-death");
    let boat = setup.boat();
    setup.start(&boat);
    let root = setup.root.path();
    let key = root.join("key");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .status()
            .unwrap()
            .success()
    );
    let port = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = root.join("sshd_config");
    fs::write(&config, format!(
        "ListenAddress 127.0.0.1\nPort {port}\nHostKey \"{key}\"\nAuthorizedKeysFile \"{key}.pub\"\nPidFile \"{root}/sshd.pid\"\nStrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\n",
        root=root.display(), key=key.display())).unwrap();
    let mut server = Process(
        Command::new("/usr/sbin/sshd")
            .args(["-D", "-e", "-f"])
            .arg(config)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            server.0.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "sshd failed to start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let ssh = root.join("bin/ssh");
    fs::rename(&ssh, ssh.with_file_name("ssh-local")).unwrap();
    // Keep the fake provider and ordinary SSH fixture. For dedicated forwards
    // run that fixture on the other end of a real OpenSSH connection.
    fs::write(&ssh, r#"#!/bin/bash
set -eu
args=("$@")
forwards=()
while (($#)); do
  if [[ "$1" == -L ]]; then forwards+=(-L "$2"); shift; fi
  shift
done
local_ssh="$TEST_ROOT/bin/ssh-local"
if ((${#forwards[@]} == 0)); then exec "$local_ssh" "${args[@]}"; fi
printf -v remote '%q ' env "BOATPARTY_RUNNER_REPO=$BOATPARTY_RUNNER_REPO" "BOATPARTY_RUNNER_STATE=$BOATPARTY_RUNNER_STATE" "$local_ssh" "${args[@]}"
exec /usr/bin/ssh -F /dev/null -T -o BatchMode=yes -o IdentitiesOnly=yes -o ExitOnForwardFailure=yes \
  -o "UserKnownHostsFile=$TEST_ROOT/known_hosts" -o StrictHostKeyChecking=yes \
  -i "$TEST_ROOT/key" -p "$TEST_PORT" "${forwards[@]}" 127.0.0.1 "$remote"
"#).unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    let public_key = fs::read_to_string(key.with_extension("pub")).unwrap();
    fs::write(
        root.join("known_hosts"),
        format!("[127.0.0.1]:{port} {public_key}"),
    )
    .unwrap();
    assert_released(port, || {
        let mut cli = setup.spawn_cmd(&boat);
        cli.env("TEST_ROOT", root)
            .env("TEST_PORT", port.to_string());
        cli
    });
}
