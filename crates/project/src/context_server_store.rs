pub mod extension;
pub mod registry;

use std::sync::Arc;

use gpui::{App, Context, Entity, EventEmitter, Subscription, WeakEntity};
use registry::ContextServerDescriptorRegistry;
use remote::RemoteClient;
use rpc::AnyProtoClient;

use crate::{
    Project,
    worktree_store::WorktreeStore,
};

pub fn init(cx: &mut App) {
    extension::init(cx);
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContextServerId(pub Arc<str>);

impl std::fmt::Display for ContextServerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContextServerStatus {
    Stopped,
}

pub struct ServerStatusChangedEvent {
    pub server_id: ContextServerId,
    pub status: ContextServerStatus,
}

impl EventEmitter<ServerStatusChangedEvent> for ContextServerStore {}

enum ContextServerStoreState {
    Local {
        _downstream_client: Option<(u64, AnyProtoClient)>,
    },
    Remote {
        _project_id: u64,
        _upstream_client: Entity<RemoteClient>,
    },
}

pub struct ContextServerStore {
    state: ContextServerStoreState,
    _worktree_store: Entity<WorktreeStore>,
    _project: Option<WeakEntity<Project>>,
    _registry: Entity<ContextServerDescriptorRegistry>,
    _subscriptions: Vec<Subscription>,
}

impl ContextServerStore {
    pub fn local(
        worktree_store: Entity<WorktreeStore>,
        weak_project: Option<WeakEntity<Project>>,
        _headless: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let registry = ContextServerDescriptorRegistry::default_global(cx);
        Self {
            state: ContextServerStoreState::Local {
                _downstream_client: None,
            },
            _worktree_store: worktree_store,
            _project: weak_project,
            _registry: registry,
            _subscriptions: Vec::new(),
        }
    }

    pub fn remote(
        project_id: u64,
        upstream_client: Entity<RemoteClient>,
        worktree_store: Entity<WorktreeStore>,
        weak_project: Option<WeakEntity<Project>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let registry = ContextServerDescriptorRegistry::default_global(cx);
        Self {
            state: ContextServerStoreState::Remote {
                _project_id: project_id,
                _upstream_client: upstream_client,
            },
            _worktree_store: worktree_store,
            _project: weak_project,
            _registry: registry,
            _subscriptions: Vec::new(),
        }
    }

    pub fn init_headless(_session: &AnyProtoClient) {}

    pub fn shared(&mut self, _project_id: u64, _client: AnyProtoClient) {
        if let ContextServerStoreState::Local {
            _downstream_client, ..
        } = &mut self.state
        {
            *_downstream_client = Some((_project_id, _client));
        }
    }

    pub fn server_ids(&self) -> &[ContextServerId] {
        &[]
    }
}
