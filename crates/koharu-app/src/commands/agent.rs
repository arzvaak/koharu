mod host;

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context as _, Result, anyhow};
use koharu_agent::{
    Account, Agent, Chat, Codex, CodexModel, Config, Control, Event, LoginEvent, RunId,
};
use parking_lot::Mutex;
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Manager as _, State, ipc::Channel};
use tauri_runtime_cef::CefRuntime;
use tokio::sync::Notify;

use self::host::KoharuHost;
use super::{Error, project::CurrentProject};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Type)]
pub struct AgentStatus {
    pub account: Option<Account>,
    pub models: Vec<CodexModel>,
    pub config: Config,
    pub running: Option<RunId>,
    pub chats: Vec<Chat>,
    pub active_chat: Option<Uuid>,
}

pub(crate) struct AgentState {
    agent: Arc<Agent<KoharuHost>>,
    runs: Mutex<HashMap<RunId, Control>>,
    login: Mutex<Option<Control>>,
    idle: Notify,
    handle: AppHandle<CefRuntime>,
    scope: tokio::sync::Mutex<Option<String>>,
    actions: tokio::sync::Mutex<()>,
}

impl AgentState {
    pub(crate) fn new(handle: AppHandle<CefRuntime>) -> Result<Self> {
        Ok(Self {
            agent: Arc::new(Agent::new(Codex::new()?, KoharuHost::new(handle.clone()))?),
            runs: Mutex::new(HashMap::new()),
            login: Mutex::new(None),
            idle: Notify::new(),
            handle,
            scope: tokio::sync::Mutex::new(None),
            actions: tokio::sync::Mutex::new(()),
        })
    }

    async fn ensure_history(&self) -> Result<()> {
        let account = self.agent.codex().account()?;
        let current = self.handle.state::<CurrentProject>();
        let project = current.project.lock().await;
        let scope = account
            .as_ref()
            .zip(project.as_ref())
            .map(|(account, project)| {
                serde_json::to_string(&(account.id.as_str(), project.info().name)).unwrap()
            });
        drop(project);
        let mut loaded = self.scope.lock().await;
        if *loaded != scope {
            if let Some(scope) = &scope {
                let root = dirs::data_dir()
                    .context("no application data directory")?
                    .join("Koharu/agent-chats");
                self.agent.open_history(&root, scope).await?;
            } else {
                self.agent.clear().await;
            }
            *loaded = scope;
        }
        Ok(())
    }

    async fn status(&self) -> Result<AgentStatus> {
        let mut account = self.agent.codex().account()?;
        let models = if account.is_some() {
            match self.agent.models().await {
                Ok(models) => models,
                Err(error) => {
                    account = self.agent.codex().account()?;
                    tracing::warn!(%error, "could not refresh Agent models; saved chats remain available");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        self.ensure_history().await?;
        let running = self.runs.lock().keys().next().copied();
        Ok(AgentStatus {
            account,
            models,
            config: self.agent.config()?,
            running,
            chats: self.agent.chats().await,
            active_chat: self.agent.selected_chat().await,
        })
    }

    pub(crate) async fn reset(&self) {
        if let Some(login) = self.login.lock().as_ref() {
            login.cancel();
        }
        for control in self.runs.lock().values() {
            control.cancel();
        }
        loop {
            let idle = self.idle.notified();
            if self.login.lock().is_none() && self.runs.lock().is_empty() {
                break;
            }
            idle.await;
        }
        self.agent.clear().await;
        *self.scope.lock().await = None;
    }

    pub(crate) fn cancel_all(&self) {
        if let Some(login) = self.login.lock().take() {
            login.cancel();
        }
        for control in self.runs.lock().drain().map(|(_, control)| control) {
            control.cancel();
        }
    }
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_agent_status(
    state: State<'_, AgentState>,
) -> std::result::Result<AgentStatus, Error> {
    Ok(state.status().await?)
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "agent_login",
    skip_all,
    fields(provider = "codex")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn login_agent(
    state: State<'_, AgentState>,
    on_event: Channel<LoginEvent>,
) -> std::result::Result<AgentStatus, Error> {
    let control = Control::default();
    {
        let mut login = state.login.lock();
        if login.is_some() {
            return Err(anyhow!("Codex sign-in is already running").into());
        }
        *login = Some(control.clone());
    }
    let result = state
        .agent
        .codex()
        .login_device(&control, |event| {
            if let LoginEvent::DeviceCode {
                verification_url, ..
            } = &event
                && let Err(error) = open::that(verification_url)
            {
                tracing::warn!(%error, "failed to open the Codex device sign-in page");
            }
            let _ = on_event.send(event);
        })
        .await;
    state.login.lock().take();
    state.idle.notify_waiters();
    result?;
    Ok(state.status().await?)
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "agent_logout",
    skip_all,
    fields(provider = "codex")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn logout_agent(
    state: State<'_, AgentState>,
) -> std::result::Result<AgentStatus, Error> {
    state.reset().await;
    state.agent.codex().logout()?;
    Ok(state.status().await?)
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "preferences_saved",
    skip_all,
    fields(setting = "agent")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn save_agent_config(
    config: Config,
    state: State<'_, AgentState>,
) -> std::result::Result<Config, Error> {
    let config = state.agent.save_config(config)?;
    tracing::info!(
        target: "koharu_metrics",
        metric = "preference_changed",
        setting = "agent",
    );
    Ok(config)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn run_agent(
    prompt: String,
    on_event: Channel<Event>,
    handle: AppHandle<CefRuntime>,
    state: State<'_, AgentState>,
) -> std::result::Result<RunId, Error> {
    let prompt = prompt.trim().to_owned();
    if prompt.is_empty() {
        return Err(anyhow!("message cannot be empty").into());
    }
    if state.agent.codex().account()?.is_none() {
        return Err(anyhow!("Codex is not signed in").into());
    }
    let _action = state.actions.lock().await;
    state.ensure_history().await?;
    let run = RunId::new();
    let control = Control::default();
    {
        let mut runs = state.runs.lock();
        if !runs.is_empty() {
            return Err(anyhow!("another agent request is already running").into());
        }
        runs.insert(run, control.clone());
    }
    let agent = state.agent.clone();
    drop(tauri::async_runtime::spawn(async move {
        let _metric = tracing::info_span!(
            target: "koharu_metrics",
            "agent_run",
            provider = "codex",
            character_count = prompt.chars().count(),
        );
        let publish_control = control.clone();
        let result = agent
            .run(run, prompt, control, |event| {
                if on_event.send(event).is_err() {
                    publish_control.cancel();
                }
            })
            .await;
        if let Err(error) = result {
            tracing::error!(%run, error = ?error, "agent request failed");
        }
        let state = handle.state::<AgentState>();
        state.runs.lock().remove(&run);
        state.idle.notify_waiters();
    }));
    Ok(run)
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "agent_cancel",
    skip_all,
    fields(provider = "codex", state = "requested")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn cancel_agent(
    run: RunId,
    state: State<'_, AgentState>,
) -> std::result::Result<(), Error> {
    state
        .runs
        .lock()
        .get(&run)
        .with_context(|| format!("agent run {run} is not active"))?
        .cancel();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn select_agent_chat(
    id: Option<Uuid>,
    state: State<'_, AgentState>,
) -> std::result::Result<AgentStatus, Error> {
    let _action = state.actions.lock().await;
    if !state.runs.lock().is_empty() || state.login.lock().is_some() {
        return Err(anyhow!("finish the current Agent request before switching chats").into());
    }
    state.ensure_history().await?;
    state.agent.choose_chat(id).await?;
    Ok(state.status().await?)
}
