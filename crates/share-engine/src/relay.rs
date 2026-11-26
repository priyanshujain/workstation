use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use share_protocol::{AGENT_PATH, AGENT_SECRET_HEADER, AgentMessage, RelayMessage};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const REGISTER_TIMEOUT: Duration = Duration::from_secs(15);

pub type Relay = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub async fn connect(
    relay_url: &str,
    secret: &str,
    hello: &AgentMessage,
) -> Result<(Relay, RelayMessage)> {
    let url = agent_url(relay_url)?;
    let mut request = url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert(AGENT_SECRET_HEADER, secret.parse()?);
    let (mut relay, _) = tokio_tungstenite::connect_async(request)
        .await
        .with_context(|| format!("could not connect to the relay at {url}"))?;
    send(&mut relay, hello).await?;
    let reply = tokio::time::timeout(REGISTER_TIMEOUT, handshake(&mut relay))
        .await
        .context("the relay did not confirm the share")??;
    Ok((relay, reply))
}

pub async fn send(relay: &mut Relay, message: &AgentMessage) -> Result<()> {
    relay
        .send(Message::Text(serde_json::to_string(message)?.into()))
        .await?;
    Ok(())
}

pub fn parse(message: &Message) -> Option<RelayMessage> {
    match message {
        Message::Text(body) => serde_json::from_str(body).ok(),
        _ => None,
    }
}

async fn handshake(relay: &mut Relay) -> Result<RelayMessage> {
    while let Some(message) = relay.next().await {
        match parse(&message?) {
            Some(
                reply @ (RelayMessage::Registered { .. }
                | RelayMessage::Managed { .. }
                | RelayMessage::Resumed { .. }
                | RelayMessage::ResumeRejected { .. }),
            ) => return Ok(reply),
            Some(RelayMessage::Stop { reason }) => bail!("the relay refused the share: {reason}"),
            _ => continue,
        }
    }
    bail!("the relay closed the connection before it confirmed the share")
}

fn agent_url(relay_url: &str) -> Result<String> {
    let base = relay_url.trim_end_matches('/');
    let base = match base.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}"),
        Some(("http", rest)) => format!("ws://{rest}"),
        Some(("ws" | "wss", _)) => base.to_string(),
        _ => bail!("relay URL {relay_url} must start with ws://, wss://, http:// or https://"),
    };
    Ok(format!("{base}{AGENT_PATH}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_http_schemes_to_websocket() {
        assert_eq!(
            agent_url("https://share.example.com/").unwrap(),
            "wss://share.example.com/agent"
        );
        assert_eq!(
            agent_url("http://127.0.0.1:8080").unwrap(),
            "ws://127.0.0.1:8080/agent"
        );
        assert_eq!(
            agent_url("wss://r.example").unwrap(),
            "wss://r.example/agent"
        );
        assert!(agent_url("r.example").is_err());
    }
}
