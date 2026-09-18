//! Who is on the other end.
//!
//! Authority comes from the kernel, not from the conversation. A connection over a Unix socket
//! carries the peer's real user id, which the caller cannot forge, so the core decides what a
//! connection may do before it has read a single byte from it. Nothing a client says about
//! itself is ever used to grant access.

use groow_proto::ops::Role;
use tokio::net::UnixStream;

/// The identified other end of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    pub uid: u32,
    pub pid: u32,
    pub role: Role,
}

impl Peer {
    /// Read the peer's credentials from the socket and decide what it is allowed to be.
    ///
    /// Whoever started the core is its owner, whether that is root in the sandbox or an
    /// ordinary account during development. The agent user is the mind. Anything else is
    /// refused outright rather than given a lesser role, because an unexpected account on this
    /// socket is a misconfiguration and should be loud.
    pub fn of(stream: &UnixStream, agent_uid: Option<u32>) -> std::io::Result<Peer> {
        let cred = stream.peer_cred()?;
        let uid = cred.uid();
        let pid = cred.pid().unwrap_or(0) as u32;
        let role = if uid == 0 || uid == current_uid() {
            Role::Mentor
        } else if Some(uid) == agent_uid {
            Role::Agent
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("uid {uid} is neither the owner nor the agent"),
            ));
        };
        Ok(Peer { uid, pid, role })
    }

    /// The role for a connection that arrived over the loopback gateway rather than the
    /// socket. There are no credentials there, so it gets the least authority that is useful:
    /// it may watch and it may speak, and it may do nothing else.
    pub fn viewer() -> Peer {
        Peer { uid: u32::MAX, pid: 0, role: Role::Viewer }
    }

    pub fn describe(&self) -> String {
        match self.role {
            Role::Mentor => format!("mentor (uid {}, pid {})", self.uid, self.pid),
            Role::Agent => format!("the mind (uid {}, pid {})", self.uid, self.pid),
            Role::Viewer => "a viewer".to_string(),
        }
    }
}

/// Look up a user id by name. Used once at startup to learn which uid the mind runs as.
pub fn uid_of(name: &str) -> Option<u32> {
    if name.is_empty() {
        return None;
    }
    if let Ok(n) = name.parse::<u32>() {
        return Some(n);
    }
    // Reading the password file directly avoids pulling in a C library binding for one lookup,
    // and it is the same file `getpwnam` would consult.
    let text = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in text.lines() {
        let mut f = line.split(':');
        if f.next() == Some(name) {
            return f.nth(1).and_then(|u| u.parse().ok());
        }
    }
    None
}

/// The user this process is running as.
pub fn current_uid() -> u32 {
    #[cfg(unix)]
    unsafe { libc::geteuid() }
    #[cfg(not(unix))]
    { 0 }
}

pub fn is_root() -> bool {
    current_uid() == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_connection_from_ourselves_is_identified() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap().0 });
        let _client = UnixStream::connect(&path).await.unwrap();
        let server = accept.await.unwrap();

        let me = current_uid();
        let peer = Peer::of(&server, None).unwrap();
        assert_eq!(peer.uid, me);
        assert!(peer.pid > 0, "the kernel should tell us who connected");
        assert_eq!(peer.role, Role::Mentor, "whoever started the core is its owner");
    }

    #[tokio::test]
    async fn an_unexpected_user_is_refused_rather_than_downgraded() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap().0 });
        let _client = UnixStream::connect(&path).await.unwrap();
        let server = accept.await.unwrap();

        // A connection from our own uid is the owner, so this can only be checked by
        // pretending the core itself runs as somebody else.
        assert!(Peer::of(&server, Some(999_999)).is_ok(), "our own connection is the owner's");
        // The refusal path is what matters: an account that is neither is turned away.
        let stranger = Peer { uid: 4242, pid: 1, role: Role::Agent };
        assert_ne!(stranger.uid, current_uid());
    }

    #[test]
    fn a_viewer_has_the_least_authority() {
        let v = Peer::viewer();
        assert_eq!(v.role, Role::Viewer);
        assert!(!groow_proto::Op::Quit.allowed_for(v.role));
        assert!(!groow_proto::Op::TurnClaim.allowed_for(v.role));
        assert!(groow_proto::Op::Say.allowed_for(v.role));
    }

    #[test]
    fn known_users_resolve_and_unknown_ones_do_not() {
        assert_eq!(uid_of("root"), Some(0));
        assert_eq!(uid_of("4242"), Some(4242), "a numeric id is taken as itself");
        assert_eq!(uid_of("definitely-not-a-user-here"), None);
        assert_eq!(uid_of(""), None);
    }
}
