//! The interactive terminal UI.

pub mod app;
pub mod input;
pub mod markdown;
pub mod theme;
pub mod transcript;
pub mod view;
pub mod wrap;

use std::io::stdout;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use futures::StreamExt;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, SharedMode};
use crate::config::{save_history, Config};
use crate::cortex::chat::Usage;
use app::{Action, App, AppEvent};
pub use transcript::Welcome;

/// What the session did, for the goodbye line.
pub struct Summary {
    pub usage: Usage,
}

/// Runs the TUI until the user quits.
pub async fn run(
    agent: Agent,
    mode: SharedMode,
    welcome: Welcome,
    history: Vec<String>,
    initial_prompt: Option<String>,
) -> crate::Result<Summary> {
    let client = agent.gateway().client().clone();
    let workspace = agent.gateway().workspace().to_string();
    let harness = agent.gateway().harness();
    let agent = Arc::new(Mutex::new(agent));
    let mut app = App::new(welcome, mode, history);
    if let Some(prompt) = initial_prompt {
        app.submit_text(prompt);
    }

    let mut terminal = ratatui::init();
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    let _ = execute!(stdout(), EnableBracketedPaste, EnableMouseCapture);
    if enhanced {
        let _ = execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let restore = move || {
        if enhanced {
            let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = execute!(stdout(), DisableBracketedPaste, DisableMouseCapture);
        ratatui::restore();
    };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableBracketedPaste, DisableMouseCapture);
        default_hook(info);
    }));

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();
    let mut terminal_events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(90));
    let mut turn: Option<CancellationToken> = None;

    let result: crate::Result<()> = async {
        loop {
            if app.dirty {
                terminal.draw(|f| view::draw(f, &mut app))?;
                app.dirty = false;
            }
            tokio::select! {
                biased;
                Some(event) = agent_rx.recv() => {
                    app.on_event(AppEvent::Agent(event));
                    while let Ok(event) = agent_rx.try_recv() {
                        app.on_event(AppEvent::Agent(event));
                    }
                }
                Some(event) = rx.recv() => {
                    // Agent events sent before this one must be shown first.
                    while let Ok(event) = agent_rx.try_recv() {
                        app.on_event(AppEvent::Agent(event));
                    }
                    app.on_event(event);
                }
                event = terminal_events.next() => match event {
                    Some(Ok(event)) => app.on_terminal(event),
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                _ = tick.tick() => app.on_tick(),
            }

            for action in app.take_actions() {
                match action {
                    Action::Submit(text) => {
                        let cancel = CancellationToken::new();
                        turn = Some(cancel.clone());
                        let (agent, tx, agent_tx) = (agent.clone(), tx.clone(), agent_tx.clone());
                        tokio::spawn(async move {
                            let outcome =
                                agent.lock().await.run_turn(text, &agent_tx, &cancel).await;
                            let _ = tx.send(AppEvent::TurnFinished(outcome));
                        });
                    }
                    Action::Interrupt => {
                        if let Some(cancel) = &turn {
                            cancel.cancel();
                        }
                    }
                    Action::FetchModels => {
                        let (client, workspace, tx) =
                            (client.clone(), workspace.clone(), tx.clone());
                        tokio::spawn(async move {
                            let result = client
                                .models(&workspace, harness)
                                .await
                                .map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::Models(result));
                        });
                    }
                    Action::SetModel(model) => {
                        let (agent, tx) = (agent.clone(), tx.clone());
                        tokio::spawn(async move {
                            let mut agent = agent.lock().await;
                            let result = match agent.gateway_mut().set_model(&model).await {
                                Ok(()) => {
                                    let model = agent.gateway().model().to_string();
                                    let mut config = Config::load();
                                    config.default_model = Some(model.clone());
                                    let _ = config.save();
                                    Ok(model)
                                }
                                Err(e) => Err(e.to_string()),
                            };
                            let _ = tx.send(AppEvent::ModelChanged(result));
                        });
                    }
                    Action::ClearConversation => {
                        let agent = agent.clone();
                        tokio::spawn(async move { agent.lock().await.reset() });
                    }
                    Action::SaveMode(mode) => {
                        let mut config = Config::load();
                        config.permission_mode = Some(mode);
                        let _ = config.save();
                    }
                    Action::Quit => {}
                }
            }
            if app.should_quit {
                break;
            }
        }
        Ok(())
    }
    .await;

    restore();
    if let Some(cancel) = turn {
        cancel.cancel();
    }
    let _ = save_history(&app.history);
    let mut agent = agent.lock().await;
    agent.gateway_mut().close().await;
    result.map(|()| Summary {
        usage: agent.usage(),
    })
}
