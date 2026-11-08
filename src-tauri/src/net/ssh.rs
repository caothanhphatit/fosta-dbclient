//! SSH tunnels (not implemented yet): the connection dialog saves the settings, connecting with SSH enabled says so.

use super::PreparedEndpoint;
use crate::{
    contract::{ConnectSecrets, ConnectionSpec, SshConfig},
    error::{AppError, AppResult},
};

pub const SSH_NOT_AVAILABLE: &str = "SSH_NOT_AVAILABLE";

pub async fn open_tunnel(_ssh: &SshConfig, _spec: &ConnectionSpec, _secrets: &ConnectSecrets) -> AppResult<PreparedEndpoint> {
    Err(AppError::coded(
        SSH_NOT_AVAILABLE,
        "SSH tunnels are not available in this build yet. Turn SSH off for this connection, or open the tunnel yourself (ssh -L) and connect to its local port.",
    ))
}
