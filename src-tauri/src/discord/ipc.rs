use std::{io, time::Duration};

use discord_rich_presence::activity::Activity;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(windows)]
use tokio::net::windows::named_pipe::NamedPipeClient as Socket;
#[cfg(unix)]
use tokio::net::UnixStream as Socket;

const TIMEOUT: Duration = Duration::from_secs(2);

pub(super) struct Client(Socket);

impl Client {
    pub(super) async fn connect(app_id: &str) -> io::Result<Self> {
        tokio::time::timeout(TIMEOUT, async {
            let mut socket = open_socket().await?;
            write_frame(&mut socket, 0, &json!({"v": 1, "client_id": app_id})).await?;
            wait_for_reply(&mut socket, None).await?;
            Ok(Self(socket))
        })
        .await?
    }

    pub(super) async fn set_activity(&mut self, activity: Option<Activity<'_>>) -> io::Result<()> {
        set_activity(&mut self.0, activity, TIMEOUT).await
    }
}

async fn set_activity(
    socket: &mut (impl AsyncRead + AsyncWrite + Unpin),
    activity: Option<Activity<'_>>,
    timeout: Duration,
) -> io::Result<()> {
    let nonce = uuid::Uuid::new_v4().to_string();
    let payload = json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id(), "activity": activity },
        "nonce": nonce,
    });
    tokio::time::timeout(timeout, async {
        write_frame(socket, 1, &payload).await?;
        wait_for_reply(socket, Some(&nonce)).await
    })
    .await?
}

#[cfg(windows)]
async fn open_socket() -> io::Result<Socket> {
    use tokio::net::windows::named_pipe::ClientOptions;

    (0..10)
        .find_map(|index| {
            ClientOptions::new()
                .open(format!(r"\\.\pipe\discord-ipc-{index}"))
                .ok()
        })
        .ok_or_else(socket_not_found)
}

#[cfg(unix)]
async fn open_socket() -> io::Result<Socket> {
    use std::{env, path::PathBuf};

    let mut roots: Vec<_> = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .into_iter()
        .filter_map(|key| {
            let path = PathBuf::from(env::var_os(key)?);
            if key == "XDG_RUNTIME_DIR" && env::var_os("SNAP").is_some() {
                return path.parent().map(PathBuf::from);
            }
            Some(path)
        })
        .collect();
    roots.push(PathBuf::from("/tmp"));
    for root in roots {
        for index in 0..10 {
            for subdir in [
                "",
                "app/com.discordapp.Discord",
                "app/dev.vencord.Vesktop",
                ".flatpak/com.discordapp.Discord/xdg-run",
                ".flatpak/dev.vencord.Vesktop/xdg-run",
                "snap.discord-canary",
                "snap.discord",
            ] {
                let path = root.join(subdir).join(format!("discord-ipc-{index}"));
                if let Ok(socket) = Socket::connect(path).await {
                    return Ok(socket);
                }
            }
        }
    }
    Err(socket_not_found())
}

fn socket_not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "Discord IPC socket not found")
}

async fn write_frame(
    socket: &mut (impl AsyncWrite + Unpin),
    opcode: u32,
    data: &Value,
) -> io::Result<()> {
    let data = serde_json::to_vec(data)?;
    let mut frame = Vec::with_capacity(8 + data.len());
    frame.extend(opcode.to_le_bytes());
    frame.extend((data.len() as u32).to_le_bytes());
    frame.extend(data);
    socket.write_all(&frame).await
}

async fn wait_for_reply(
    socket: &mut (impl AsyncRead + AsyncWrite + Unpin),
    nonce: Option<&str>,
) -> io::Result<()> {
    loop {
        let opcode = socket.read_u32_le().await?;
        let len = socket.read_u32_le().await?;
        if len > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Discord IPC frame too large",
            ));
        }
        let mut data = vec![0; len as usize];
        socket.read_exact(&mut data).await?;
        let reply: Value = serde_json::from_slice(&data)?;
        match opcode {
            1 if nonce.map_or_else(|| reply["evt"] == "READY", |id| reply["nonce"] == id) => {
                return if reply["evt"] == "ERROR" {
                    Err(io::Error::other("Discord rejected the activity"))
                } else {
                    Ok(())
                };
            }
            2 => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "Discord closed IPC",
                ))
            }
            3 => write_frame(socket, 4, &reply).await?,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clear_sends_null_and_drains_the_reply() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let clear = set_activity(&mut client, None, TIMEOUT);
        let reply = async {
            assert_eq!(server.read_u32_le().await.unwrap(), 1);
            let len = server.read_u32_le().await.unwrap();
            let mut data = vec![0; len as usize];
            server.read_exact(&mut data).await.unwrap();
            let message: Value = serde_json::from_slice(&data).unwrap();
            assert_eq!(message["cmd"], "SET_ACTIVITY");
            assert_eq!(message["args"].get("activity"), Some(&Value::Null));
            write_frame(&mut server, 1, &json!({"nonce": message["nonce"]}))
                .await
                .unwrap();
        };
        let (result, ()) = tokio::join!(clear, reply);
        result.unwrap();
    }

    #[tokio::test]
    async fn clear_times_out_when_discord_stops_responding() {
        let (mut client, _server) = tokio::io::duplex(4096);
        let error = set_activity(&mut client, None, Duration::from_millis(25))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
