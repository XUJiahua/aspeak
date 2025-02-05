use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::errors::ConnectError;
use crate::msg;
use crate::net::WsStream;
use crate::{interpolate_ssml, msg::WebSocketMessage, AudioFormat, TextOptions};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use hyper::header::InvalidHeaderValue;
use log::{debug, info, warn};

use strum::AsRefStr;
use tokio_tungstenite::tungstenite::protocol::Message;
use uuid::Uuid;

/// The main struct for interacting with the Azure Speech Service.
pub struct WebsocketSynthesizer {
    pub(super) audio_format: AudioFormat,
    pub(super) stream: WsStream,
    pub audio_metadata: Option<Vec<String>>,
}

impl WebsocketSynthesizer {
    /// Synthesize the given SSML into audio([`Vec<u8>`]).
    pub async fn synthesize_ssml(
        &mut self,
        ssml: &str,
    ) -> Result<Vec<u8>, WebsocketSynthesizerError> {
        let uuid = Uuid::new_v4();
        let request_id = uuid.as_simple();
        let now = Utc::now();
        let synthesis_context = format!(
            r#"{{"synthesis":{{"audio":{{"metadataOptions":{{"sentenceBoundaryEnabled":false,"wordBoundaryEnabled":true,"sessionEndEnabled":false}},"outputFormat":"{}"}}}}}}"#,
            Into::<&str>::into(self.audio_format)
        );
        self.stream.send(Message::Text(format!(
            "Path: synthesis.context\r\nX-RequestId: {request_id}\r\nX-Timestamp: {now:?}Content-Type: application/json\r\n\r\n{synthesis_context}", 
            request_id = &request_id)),
        ).await?;
        info!("Before sending the SSML to the server");
        self.stream.send(Message::Text(format!(
            "Path: ssml\r\nX-RequestId: {request_id}\r\nX-Timestamp: {now:?}\r\nContent-Type: application/ssml+xml\r\n\r\n{ssml}"
        ))).await?;
        let mut buffer = Vec::new();
        let mut audio_metadata = Vec::new();
        while let Some(raw_msg) = self.stream.next().await.transpose()? {
            let msg = WebSocketMessage::try_from(&raw_msg)?;
            match msg {
                WebSocketMessage::TurnStart | WebSocketMessage::Response { body: _ } => continue,
                WebSocketMessage::Audio { data } => {
                    buffer.extend_from_slice(data);
                }
                WebSocketMessage::AudioMetadata { body } => {
                    audio_metadata.push(body.to_string());
                }
                WebSocketMessage::TurnEnd => {
                    break;
                }
                WebSocketMessage::Close(frame) => {
                    return Err(frame.map_or_else(
                        || {
                            WebsocketSynthesizerError::connection_closed(
                                "Unknown".to_string(),
                                "The server closed the connection without a reason".to_string(),
                            )
                        },
                        |fr| {
                            WebsocketSynthesizerError::connection_closed(
                                fr.code.to_string(),
                                fr.reason.to_string(),
                            )
                        },
                    ));
                }
                msg => warn!("Received a message that is not handled: {:?}", msg),
            }
        }
        self.audio_metadata = Some(audio_metadata);

        Ok(buffer)
    }

    /// Synthesize the given text into audio([`Vec<u8>`]).
    /// This is a convenience method that interpolates the SSML for you.
    pub async fn synthesize_text(
        &mut self,
        text: impl AsRef<str>,
        options: &TextOptions<'_>,
    ) -> Result<Vec<u8>, WebsocketSynthesizerError> {
        debug!("Synthesizing text: {}", text.as_ref());
        let ssml = interpolate_ssml(text, options)?;
        self.synthesize_ssml(&ssml).await
    }
}

/// Errors that can occur when creating and using a [`WebsocketSynthesizer`].
#[derive(Debug)]
#[non_exhaustive]
pub struct WebsocketSynthesizerError {
    pub kind: WebsocketSynthesizerErrorKind,
    pub(crate) source: Option<anyhow::Error>,
}

impl WebsocketSynthesizerError {
    fn connection_closed(code: String, reason: String) -> Self {
        Self {
            kind: WebsocketSynthesizerErrorKind::WebsocketConnectionClosed { code, reason },
            source: None,
        }
    }
}

impl Display for WebsocketSynthesizerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        use WebsocketSynthesizerErrorKind::*;
        write!(f, "ws synthesizer error: ")?;
        match &self.kind {
            WebsocketConnectionClosed { code, reason } => {
                write!(
                    f,
                    "the websocket connection was closed with code {} and reason {}",
                    code, reason
                )
            }
            InvalidMessage => write!(f, "aspeak cannot handle this message. Please report this bug to https://github.com/kxxt/aspeak/issues."),
            _ => write!(f, "{} error", self.kind.as_ref()),
        }
    }
}

impl Error for WebsocketSynthesizerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(|e| e.as_ref() as _)
    }
}

#[cfg(feature = "python")]
impl From<WebsocketSynthesizerError> for pyo3::PyErr {
    fn from(value: WebsocketSynthesizerError) -> Self {
        pyo3::exceptions::PyOSError::new_err(format!("{:?}", color_eyre::Report::from(value)))
    }
}

#[derive(Debug, PartialEq, Clone, AsRefStr)]
#[non_exhaustive]
#[strum(serialize_all = "title_case")]
pub enum WebsocketSynthesizerErrorKind {
    /// Failed to connect to the endpoint.
    Connect,
    /// The websocket connection was closed.
    WebsocketConnectionClosed { code: String, reason: String },
    /// Other websocket errors.
    Websocket,
    /// The request was invalid, either caught early by us or indicated by a BadRequest response from the server.
    InvalidRequest,
    /// An invalid websocket message was received.
    InvalidMessage,
    /// Errors that occur when interpolating SSML.
    Ssml,
}

macro_rules! impl_from_for_ws_synthesizer_error {
    ($error_type:ty, $error_kind:ident) => {
        impl From<$error_type> for WebsocketSynthesizerError {
            fn from(e: $error_type) -> Self {
                Self {
                    kind: WebsocketSynthesizerErrorKind::$error_kind,
                    source: Some(e.into()),
                }
            }
        }
    };
}

impl_from_for_ws_synthesizer_error!(InvalidHeaderValue, InvalidRequest);
impl_from_for_ws_synthesizer_error!(url::ParseError, InvalidRequest);
impl_from_for_ws_synthesizer_error!(ConnectError, Connect);
impl_from_for_ws_synthesizer_error!(tokio_tungstenite::tungstenite::Error, Websocket);
impl_from_for_ws_synthesizer_error!(crate::ssml::SsmlError, Ssml);

impl From<msg::ParseError> for WebsocketSynthesizerError {
    fn from(e: msg::ParseError) -> Self {
        Self {
            kind: WebsocketSynthesizerErrorKind::InvalidMessage,
            source: Some(e.into()),
        }
    }
}
