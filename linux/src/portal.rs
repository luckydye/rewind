//! `org.freedesktop.portal.ScreenCast`: the only way to capture a Wayland
//! screen. The portal shows its own picker, then hands back one PipeWire node
//! and an fd on the session's remote; `pipewiresrc` consumes them.
//!
//! The session has to stay open for as long as the stream is used — closing it
//! stops the capture. It is held here, in `PortalSession`, and closed when the
//! pipeline is torn down.

use std::os::fd::OwnedFd;

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::{PersistMode, Session};

pub struct PortalSession {
    pub node_id: u32,
    /// The stream size in compositor coordinates, and the position of the
    /// output in the layout. Wayland has no physical pixel size until a frame
    /// arrives, so this is what the first guess at the encoder size uses.
    pub size: (u32, u32),
    pub position: Option<(i32, i32)>,
    /// What to pass to `select_sources` next time to skip the picker.
    pub restore_token: Option<String>,
    /// The PipeWire remote the node lives on, kept for the pipeline.
    pub remote: OwnedFd,
    session: Session<'static, Screencast<'static>>,
}

impl PortalSession {
    pub async fn close(self) {
        let _ = self.session.close().await;
    }
}

fn describe(error: ashpd::Error) -> String {
    use ashpd::desktop::ResponseError;
    match error {
        ashpd::Error::Response(ResponseError::Cancelled) => "screen sharing was declined at the portal picker".into(),
        other => format!("org.freedesktop.portal.ScreenCast: {other}"),
    }
}

/// Shows the picker (unless `token` restores a previous choice) and starts a
/// session. Blocks on the user's answer, which is why callers must not hold the
/// recorder's lock.
pub async fn open(cursor: bool, token: Option<String>) -> Result<PortalSession, String> {
    let screencast: Screencast<'static> = Screencast::new().await.map_err(describe)?;
    let session = screencast.create_session().await.map_err(describe)?;

    let cursor_mode = if cursor { CursorMode::Embedded } else { CursorMode::Hidden };
    screencast
        .select_sources(
            &session,
            cursor_mode,
            SourceType::Monitor.into(),
            false,
            token.as_deref(),
            PersistMode::ExplicitlyRevoked,
        )
        .await
        .map_err(describe)?;

    let response = screencast.start(&session, None).await.map_err(describe)?.response().map_err(describe)?;

    let stream = response.streams().first().ok_or("the portal returned no screen cast stream")?;
    let node_id = stream.pipe_wire_node_id();
    let size = stream
        .size()
        .map(|(width, height)| (width.max(1) as u32, height.max(1) as u32))
        .ok_or("the portal did not report the stream size")?;
    let restore_token = response.restore_token().map(str::to_string);

    let remote = screencast.open_pipe_wire_remote(&session).await.map_err(describe)?;

    Ok(PortalSession { node_id, size, position: stream.position(), restore_token, remote, session })
}
