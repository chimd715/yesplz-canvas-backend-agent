//! Trusted offline preparation, not a filesystem freeze or a public restore API.
use std::io;
#[cfg(target_os = "linux")]
use std::path::Component;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Owner/build binding comes from the externally verified archive registration.
/// The local checkpoint alone does not authenticate either of these values.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenNativeSession {
    pub owner_id: String,
    pub runtime_build_sha256: String,
    pub manifest_json: String,
    pub session_json: String,
}

/// Trusted arguments, deliberately separate from deserialized archive contents.
pub struct NativeContinuationTarget<'a> {
    pub owner_id: &'a str,
    pub expected_runtime_build_sha256: &'a str,
    pub source_conversation_id: &'a str,
    pub source_turn_id: &'a str,
    pub conversation_id: &'a str,
    pub source_workspace: &'a Path,
    pub restored_workspace: &'a Path,
    pub session_directory: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedNativeContinuation {
    pub conversation_id: String,
    pub source_conversation_id: String,
    pub source_turn_id: String,
    pub native_state_sha256: String,
    pub session_bytes: usize,
}

#[cfg(target_os = "linux")]
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) fn load_prepared_native_continuation(
    directory: &Path,
    id: &str,
    workspace: &str,
) -> io::Result<aion_agent::session::Session> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (directory, id, workspace);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Native continuation preparation requires Linux",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        if !super::valid_id(id) {
            return Err(invalid("Invalid continuation identity"));
        }
        let bytes = super::filesystem::read_prepared_session(directory, id, super::MAX_SESSION_BYTES)?;
        let session: aion_agent::session::Session =
            serde_json::from_slice(&bytes).map_err(|_| invalid("Invalid prepared native session"))?;
        if session.id != id || session.cwd != workspace || session.forked_from.is_none() {
            return Err(invalid("Prepared native session identity or workspace mismatch"));
        }
        Ok(session)
    }
}

#[cfg(target_os = "linux")]
fn validate(
    frozen: &FrozenNativeSession,
    target: &NativeContinuationTarget<'_>,
) -> io::Result<aion_agent::session::Session> {
    use aion_agent::session::Session;
    use aion_types::message::Role;
    use sha2::{Digest, Sha256};
    let sha =
        |value: &str| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    if ![
        target.owner_id,
        target.source_conversation_id,
        target.source_turn_id,
        target.conversation_id,
    ]
    .into_iter()
    .all(super::valid_id)
        || frozen.owner_id != target.owner_id
        || target.source_conversation_id == target.conversation_id
        || !sha(target.expected_runtime_build_sha256)
        || frozen.runtime_build_sha256 != target.expected_runtime_build_sha256
    {
        return Err(invalid("Frozen native owner, build or identity mismatch"));
    }
    if frozen.manifest_json.len() > 16 * 1024 || frozen.session_json.len() as u64 > super::MAX_SESSION_BYTES {
        return Err(invalid("Frozen native checkpoint exceeds limits"));
    }
    let manifest: super::CheckpointManifest =
        serde_json::from_str(&frozen.manifest_json).map_err(|_| invalid("Invalid frozen native manifest"))?;
    if manifest.schema_version != 1
        || manifest.backend != "aionrs"
        || manifest.backend_version != "0.2.11"
        || manifest.conversation_id != target.source_conversation_id
        || manifest.session_id != target.source_conversation_id
        || manifest.turn_id != target.source_turn_id
        || !manifest.local_only
        || manifest.session_bytes != frozen.session_json.len()
        || manifest.session_sha256 != hex::encode(Sha256::digest(frozen.session_json.as_bytes()))
    {
        return Err(invalid("Frozen native manifest does not match the selected checkpoint"));
    }
    let session: Session =
        serde_json::from_str(&frozen.session_json).map_err(|_| invalid("Invalid frozen native session"))?;
    let last = session
        .messages
        .last()
        .ok_or_else(|| invalid("Frozen native session is empty"))?;
    if session.id != target.source_conversation_id
        || last.role != Role::Assistant
        || last.turn_id.as_deref() != Some(target.source_turn_id)
        || Path::new(&session.cwd) != target.source_workspace
    {
        return Err(invalid(
            "Frozen native session does not match the selected conversation and turn",
        ));
    }
    let absolute = |path: &Path| {
        path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
    };
    if !absolute(target.source_workspace)
        || !absolute(target.restored_workspace)
        || !absolute(target.session_directory)
        || target.restored_workspace.starts_with(target.source_workspace)
        || target.source_workspace.starts_with(target.restored_workspace)
        || target.restored_workspace.starts_with(target.session_directory)
        || target.session_directory.starts_with(target.restored_workspace)
    {
        return Err(invalid(
            "Restored workspace must be isolated from the source and native session directory",
        ));
    }
    // A successful frozen checkpoint must not depend on the ordinary resume
    // repair dropping malformed history. Never silently rewrite that proof.
    let mut messages = session.messages.clone();
    if crate::manager::aionrs::sanitize_session_messages(&mut messages) != 0 {
        return Err(invalid("Frozen native session requires history repair"));
    }
    Ok(session)
}

#[cfg(target_os = "linux")]
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PreparationProof {
    owner_id: String,
    runtime_build_sha256: String,
    manifest_sha256: String,
    session_sha256: String,
    workspace: String,
    prepared: PreparedNativeContinuation,
}

#[cfg(target_os = "linux")]
fn proof(
    frozen: &FrozenNativeSession,
    target: &NativeContinuationTarget<'_>,
    prepared: PreparedNativeContinuation,
) -> io::Result<PreparationProof> {
    use sha2::{Digest, Sha256};
    Ok(PreparationProof {
        owner_id: target.owner_id.into(),
        runtime_build_sha256: target.expected_runtime_build_sha256.into(),
        manifest_sha256: hex::encode(Sha256::digest(frozen.manifest_json.as_bytes())),
        session_sha256: hex::encode(Sha256::digest(frozen.session_json.as_bytes())),
        workspace: target
            .restored_workspace
            .to_str()
            .ok_or_else(|| invalid("Invalid restored workspace path"))?
            .into(),
        prepared,
    })
}

/// Read-only recovery of an exact completed preparation. An advanced or missing
/// native session is not a prepared target and cannot be silently recreated.
pub fn recover_frozen_native_continuation(
    frozen: &FrozenNativeSession,
    target: &NativeContinuationTarget<'_>,
) -> io::Result<PreparedNativeContinuation> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (frozen, target);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Native continuation preparation requires Linux",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        use sha2::{Digest, Sha256};
        let source = validate(frozen, target)?;
        let (bytes, metadata) = super::filesystem::read_preparation(
            target.session_directory,
            target.conversation_id,
            target.restored_workspace,
            super::MAX_SESSION_BYTES,
        )?;
        let stored: PreparationProof =
            serde_json::from_slice(&metadata).map_err(|_| invalid("Invalid native preparation proof"))?;
        let prepared = PreparedNativeContinuation {
            conversation_id: target.conversation_id.into(),
            source_conversation_id: target.source_conversation_id.into(),
            source_turn_id: target.source_turn_id.into(),
            native_state_sha256: hex::encode(Sha256::digest(&bytes)),
            session_bytes: bytes.len(),
        };
        if stored != proof(frozen, target, prepared.clone())? {
            return Err(invalid("Native preparation proof mismatch"));
        }
        let loaded: aion_agent::session::Session =
            serde_json::from_slice(&bytes).map_err(|_| invalid("Invalid prepared native session"))?;
        // Verify preserved semantics too; a rewritten sidecar cannot authorize
        // changed history or provider settings, even with a matching checksum.
        let mut expected = source;
        expected.root_id = Some(expected.root_id.clone().unwrap_or_else(|| expected.id.clone()));
        expected.forked_from = Some(expected.id.clone());
        expected.id = target.conversation_id.into();
        expected.cwd = stored.workspace;
        expected.created_at = loaded.created_at;
        expected.updated_at = loaded.updated_at;
        if serde_json::to_value(expected).map_err(io::Error::other)?
            != serde_json::to_value(loaded).map_err(io::Error::other)?
        {
            return Err(invalid("Prepared native session has changed"));
        }
        Ok(prepared)
    }
}

/// The caller must authorize the owner and verify the complete outer archive,
/// restore its workspace/dependencies, then import the frozen DB rows. This
/// operation does not read a mutable parent, start an agent, or change a head.
/// Files remain as unreferenced private orphans if the later DB import fails.
pub fn prepare_frozen_native_continuation(
    frozen: &FrozenNativeSession,
    target: &NativeContinuationTarget<'_>,
) -> io::Result<PreparedNativeContinuation> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (frozen, target);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Native continuation preparation requires Linux",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        use aion_agent::session::{ForkBoundary, SessionManager};
        use sha2::{Digest, Sha256};

        let session = validate(frozen, target)?;
        let workspace = target
            .restored_workspace
            .to_str()
            .ok_or_else(|| invalid("Invalid restored workspace path"))?;
        let files = super::filesystem::ContinuationFiles::new(target.session_directory, target.restored_workspace)?;
        let manager = SessionManager::new(files.manager_path(), 1);
        // Verified: pinned aionrs v0.2.11 session.rs::fork_from clones supplied
        // bytes' parsed Session and runs retention only within this fresh store.
        let mut continuation = manager
            .fork_from(&session, Some(target.conversation_id), ForkBoundary::Head)
            .map_err(|_| invalid("Frozen native session fork failed"))?;
        continuation.cwd = workspace.to_owned();
        manager
            .save(&continuation)
            .map_err(|_| invalid("Prepared native session save failed"))?;
        let loaded = manager
            .load(target.conversation_id)
            .map_err(|_| invalid("Prepared native session cannot be loaded"))?;
        let bytes = serde_json::to_vec(&loaded).map_err(|_| invalid("Prepared native session serialization failed"))?;
        if serde_json::to_value(&loaded).map_err(io::Error::other)?
            != serde_json::to_value(&continuation).map_err(io::Error::other)?
        {
            return Err(invalid("Prepared native session differs after persistence"));
        }
        let prepared = PreparedNativeContinuation {
            conversation_id: target.conversation_id.to_owned(),
            source_conversation_id: target.source_conversation_id.to_owned(),
            source_turn_id: target.source_turn_id.to_owned(),
            native_state_sha256: hex::encode(Sha256::digest(&bytes)),
            session_bytes: bytes.len(),
        };
        let metadata = serde_json::to_vec(&proof(frozen, target, prepared.clone())?).map_err(io::Error::other)?;
        files.install(target.conversation_id, &bytes, &metadata)?;
        Ok(prepared)
    }
}
