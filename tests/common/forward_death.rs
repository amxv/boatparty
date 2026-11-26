//! The target's SSH greeting proves bytes traversed the forwarded channel.
//! Keep the channel open across forced parent death and fixed-port reuse.
use std::io::{BufRead, BufReader, Read};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub struct Process(pub Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(mut command: Command, args: &[String]) -> Process {
    let mut client = Process(
        command
            .arg("--json")
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = client.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let frames = serde_json::Deserializer::from_reader(BufReader::new(stdout))
            .into_iter::<serde_json::Value>();
        for frame in frames {
            let frame = frame.unwrap();
            if frame["forward"].is_object() || frame["event"] == "ready" {
                let _ = tx.send(());
                break;
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(30))
        .expect("client never reported a ready forward");
    client
}

fn channel(port: u16) -> BufReader<TcpStream> {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut stream = BufReader::new(stream);
    let mut banner = String::new();
    stream.read_line(&mut banner).unwrap();
    assert!(banner.starts_with("SSH-"), "{banner}");
    stream
}

pub fn assert_released(target: u16, cli: impl Fn() -> Command) {
    for serve in [true, false] {
        let port = TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let forward = [
            "forward".into(),
            target.to_string(),
            "--local".into(),
            port.to_string(),
        ];
        let server = [
            "serve".into(),
            "--forward".into(),
            format!("{port}:{target}"),
            "--ready-port".into(),
            target.to_string(),
            "--".into(),
            "sleep".into(),
            "60".into(),
        ];
        let mut client = start(cli(), if serve { &server } else { &forward });
        let mut open = channel(port);
        client.0.kill().unwrap();
        client.0.wait().unwrap();
        let _replacement = start(cli(), &forward);
        let _replacement_channel = channel(port);
        match open.read(&mut [0]) {
            Ok(0) => {}
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            result => panic!("the old forwarding channel did not close: {result:?}"),
        }
    }
}
