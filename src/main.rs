use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use ironize::agent::{Agent, AgentEvent, Decision, PermissionMode, SharedMode, TurnOutcome};
use ironize::auth::{self, BrowserLogin};
use ironize::config::{load_history, Config};
use ironize::cortex::{CortexClient, Harness};
use ironize::gateway::Gateway;
use ironize::session::{Session, SessionStore, BASE_URL_ENV, TOKEN_ENV};
use ironize::tools::ToolContext;
use ironize::tui::{self, Welcome};
use ironize::{Error, DEFAULT_BASE_URL};

/// Ironize uses the OpenAI Chat Completions protocol on the Cortex gateway.
const HARNESS: Harness = Harness::Opencode;

#[derive(Parser)]
#[command(
    name = "ironize",
    version,
    about = "Ironize: an AI agent harness for your terminal, powered by Mindify Cortex",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    agent: AgentArgs,
    /// Start the session with this prompt.
    prompt: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in to Mindify Cortex.
    Login(LoginArgs),
    /// Sign out: revoke the session on Cortex and delete it locally.
    Logout,
    /// Show whether Ironize is signed in, verifying the session with Cortex.
    Status,
    /// List gateway models available to this account.
    Models {
        /// `personal` or an organization ID.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Run a task non-interactively and print the result.
    Run {
        #[command(flatten)]
        agent: AgentArgs,
        /// Approve every tool call (same as --permission-mode yolo).
        #[arg(long, short = 'y')]
        yes: bool,
        /// The task.
        #[arg(required = true, num_args = 1..)]
        task: Vec<String>,
    },
}

#[derive(Args, Clone, Default)]
struct AgentArgs {
    /// Gateway model ID (see `ironize models`).
    #[arg(long, short)]
    model: Option<String>,
    /// `personal` or an organization ID.
    #[arg(long)]
    workspace: Option<String>,
    /// When to ask before acting.
    #[arg(long, value_enum)]
    permission_mode: Option<PermissionMode>,
    /// Never ask before acting (same as --permission-mode yolo).
    #[arg(long)]
    yolo: bool,
}

#[derive(Args)]
struct LoginArgs {
    /// Cortex deployment to sign in to.
    #[arg(long, env = BASE_URL_ENV, default_value = DEFAULT_BASE_URL)]
    base_url: String,
    /// Print the sign-in URL instead of opening a browser.
    #[arg(long)]
    no_browser: bool,
    /// Sign in headlessly with email and password (prompted, or read from
    /// IRONIZE_PASSWORD).
    #[arg(long, conflicts_with = "token_stdin")]
    email: Option<String>,
    /// Read an existing Cortex session token from stdin.
    #[arg(long)]
    token_stdin: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let store = SessionStore::default_location()?;
    match cli.command {
        None => interactive(&store, cli.agent, cli.prompt).await,
        Some(Command::Login(args)) => login(&store, args).await.map(|_| ExitCode::SUCCESS),
        Some(Command::Logout) => logout(&store).await.map(|()| ExitCode::SUCCESS),
        Some(Command::Status) => status(&store).await.map(|()| ExitCode::SUCCESS),
        Some(Command::Models { workspace }) => {
            let client = signed_in(&store)?;
            let workspace = workspace
                .or(Config::load().workspace)
                .unwrap_or_else(|| "personal".into());
            let models = client.models(&workspace, HARNESS).await?;
            if models.is_empty() {
                println!("No models in workspace {workspace:?} are available to Ironize.");
            }
            for model in models {
                let tools = if model.supports_tools {
                    "tools"
                } else {
                    "text only"
                };
                println!(
                    "{:<40} {:<14} {}  ({tools})",
                    model.id, model.provider, model.display_name
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Run {
            mut agent,
            yes,
            task,
        }) => {
            agent.yolo |= yes;
            headless(&store, agent, task.join(" ")).await
        }
    }
}

// ----- sessions --------------------------------------------------------------

async fn login(store: &SessionStore, args: LoginArgs) -> Result<Session> {
    let session = if args.token_stdin {
        let mut token = String::new();
        std::io::stdin().lock().read_line(&mut token)?;
        auth::token_login(&args.base_url, token.trim()).await?
    } else if let Some(email) = args.email {
        let password = match std::env::var("IRONIZE_PASSWORD") {
            Ok(password) => password,
            Err(_) => rpassword::prompt_password(format!("Password for {email}: "))?,
        };
        auth::password_login(&args.base_url, &email, &password).await?
    } else {
        browser_login(&args.base_url, !args.no_browser).await?
    };
    store.save(&session)?;
    println!(
        "Signed in to {} as {}.",
        session.base_url,
        describe_user(&session)
    );
    Ok(session)
}

async fn browser_login(base_url: &str, open_browser: bool) -> Result<Session> {
    let open_browser = open_browser && std::io::stdout().is_terminal();
    Ok(auth::browser_login(BrowserLogin::new(base_url), |prompt| {
        let url = prompt.authorize_url.as_str();
        if open_browser && open::that_detached(url).is_ok() {
            eprintln!("Opening your browser to sign in to Mindify Cortex…\nIf it did not open, visit:\n  {url}");
        } else {
            eprintln!("Open this URL to sign in to Mindify Cortex:\n  {url}");
        }
        eprintln!("Waiting for the browser…");
    })
    .await?)
}

async fn logout(store: &SessionStore) -> Result<()> {
    if let Some(session) = store.load_saved()? {
        // Revoke server-side first so a copied session file stops working too.
        // A dead or unreachable server must not keep the user signed in locally.
        if let Err(error) = CortexClient::new(session)?.sign_out().await {
            eprintln!("warning: could not revoke the session on Cortex: {error}");
        }
    }
    store.clear()?;
    println!("Signed out.");
    if std::env::var_os(TOKEN_ENV).is_some() {
        eprintln!("note: {TOKEN_ENV} is set in the environment and still signs Ironize in.");
    }
    Ok(())
}

async fn status(store: &SessionStore) -> Result<()> {
    let client = signed_in(store)?;
    let verified = client.verify().await?;
    let session = Session {
        user: Some(verified.user),
        ..client.session().clone()
    };
    println!(
        "Signed in to {} as {}.",
        session.base_url,
        describe_user(&session)
    );
    if let Some(expires_at) = verified.session.expires_at {
        println!("Session expires {expires_at}.");
    }
    if std::env::var_os(TOKEN_ENV).is_some() {
        println!("Using {TOKEN_ENV} from the environment.");
    } else {
        println!("Session file: {}", store.path().display());
    }
    Ok(())
}

fn signed_in(store: &SessionStore) -> Result<CortexClient> {
    match store.load()? {
        Some(session) => Ok(CortexClient::new(session)?),
        None => bail!("not signed in; run `ironize login`"),
    }
}

fn describe_user(session: &Session) -> String {
    match &session.user {
        Some(user) => match (&user.name, &user.email) {
            (Some(name), Some(email)) if !name.is_empty() => format!("{name} <{email}>"),
            (_, Some(email)) => email.clone(),
            _ => user.id.clone(),
        },
        None => "an unknown user".into(),
    }
}

// ----- agent sessions --------------------------------------------------------

/// Signs in if needed (interactively), verifies the session, picks a model and
/// connects to the gateway.
async fn prepare(
    store: &SessionStore,
    args: &AgentArgs,
    interactive: bool,
) -> Result<(Agent, SharedMode, Welcome)> {
    let base_url = std::env::var(BASE_URL_ENV).unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
    let mut session = match store.load()? {
        Some(session) => session,
        None if interactive => {
            eprintln!("Welcome to Ironize! Sign in to Mindify Cortex to get started.");
            let session = browser_login(&base_url, true).await?;
            store.save(&session)?;
            session
        }
        None => bail!("not signed in; run `ironize login` or set {TOKEN_ENV}"),
    };
    let client = CortexClient::new(session.clone())?;
    match client.verify().await {
        Ok(verified) => session.user = Some(verified.user),
        Err(Error::Unauthenticated(_)) if interactive && std::env::var_os(TOKEN_ENV).is_none() => {
            eprintln!("Your Cortex session has expired. Sign in again to continue.");
            session = browser_login(&session.base_url, true).await?;
            store.save(&session)?;
        }
        Err(e) => return Err(e).context("could not verify your Cortex session"),
    }
    let client = CortexClient::new(session.clone())?;

    let config = Config::load();
    let workspace = args
        .workspace
        .clone()
        .or(config.workspace.clone())
        .unwrap_or_else(|| "personal".into());
    let explicit = args.model.clone();
    let model = match explicit.clone().or(config.default_model.clone()) {
        Some(model) => model,
        None => first_model(&client, &workspace).await?,
    };
    let mut gateway = Gateway::new(client.clone(), &workspace, HARNESS, model);
    if let Err(error) = gateway.connect().await {
        // A remembered default may have been retired; fall back unless the
        // user asked for this model explicitly.
        if explicit.is_some() || matches!(error, Error::Unauthenticated(_)) {
            return Err(error).context("could not start a gateway session");
        }
        let fallback = first_model(&client, &workspace).await?;
        eprintln!(
            "note: {} is unavailable ({error}); using {fallback}",
            gateway.model()
        );
        gateway = Gateway::new(client.clone(), &workspace, HARNESS, fallback);
        gateway
            .connect()
            .await
            .context("could not start a gateway session")?;
    }

    let mode = if args.yolo {
        PermissionMode::Yolo
    } else {
        args.permission_mode
            .or(config.permission_mode)
            .unwrap_or_default()
    };
    let mode = SharedMode::new(mode);
    let cwd = std::env::current_dir()?;
    let welcome = Welcome {
        user: describe_user(&session),
        model: gateway.model().to_string(),
        workspace,
        cwd: cwd.display().to_string(),
    };
    let agent = Agent::new(gateway, ToolContext::new(cwd), mode.clone());
    Ok((agent, mode, welcome))
}

async fn first_model(client: &CortexClient, workspace: &str) -> Result<String> {
    let models = client
        .models(workspace, HARNESS)
        .await
        .context("could not list models")?;
    models
        .iter()
        .find(|m| m.supports_tools)
        .or(models.first())
        .map(|m| m.id.clone())
        .with_context(|| format!("no models in workspace {workspace:?} are available to Ironize"))
}

async fn interactive(
    store: &SessionStore,
    args: AgentArgs,
    prompt: Vec<String>,
) -> Result<ExitCode> {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        bail!("the interactive UI needs a terminal; use `ironize run <task>` for scripts");
    }
    let (agent, mode, welcome) = prepare(store, &args, true).await?;
    let prompt = (!prompt.is_empty()).then(|| prompt.join(" "));
    let summary = tui::run(agent, mode, welcome, load_history(), prompt).await?;
    let usage = summary.usage;
    if usage.input_tokens + usage.output_tokens > 0 {
        eprintln!(
            "Session used {} input and {} output tokens.",
            usage.input_tokens, usage.output_tokens
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Runs one task without the TUI: the answer goes to stdout, progress to stderr.
async fn headless(store: &SessionStore, args: AgentArgs, task: String) -> Result<ExitCode> {
    let (mut agent, _mode, _welcome) = prepare(store, &args, false).await?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let interrupt = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            interrupt.cancel();
        }
    });

    let turn = async {
        let outcome = agent.run_turn(task, &tx, &cancel).await;
        drop(tx);
        outcome
    };
    let printer = async {
        let mut stdout = std::io::stdout();
        let mut mid_line = false;
        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::TextDelta(text) => {
                    print!("{text}");
                    let _ = stdout.flush();
                    mid_line = !text.ends_with('\n');
                }
                AgentEvent::ToolStarted { label, subject, .. } => {
                    if mid_line {
                        println!();
                        mid_line = false;
                    }
                    eprintln!("● {label}({})", subject.lines().next().unwrap_or_default());
                }
                AgentEvent::ToolFinished { output, .. } => eprintln!("  ⎿ {}", output.brief),
                AgentEvent::Permission(request) => {
                    let decision = ask_permission(request.label, &request.subject).await;
                    request.respond(decision);
                }
                AgentEvent::Thinking | AgentEvent::Todos(_) | AgentEvent::Usage(_) => {}
            }
        }
        if mid_line {
            println!();
        }
    };
    let (outcome, ()) = tokio::join!(turn, printer);
    agent.gateway_mut().close().await;
    match outcome {
        TurnOutcome::Completed => Ok(ExitCode::SUCCESS),
        TurnOutcome::Cancelled => {
            eprintln!("Interrupted.");
            Ok(ExitCode::from(130))
        }
        TurnOutcome::Denied => {
            eprintln!("Stopped: a tool call was declined. Re-run with --yes to allow tools.");
            Ok(ExitCode::from(2))
        }
        TurnOutcome::StepLimit => bail!("stopped after reaching the step limit"),
        TurnOutcome::Failed(e) => bail!(e),
    }
}

/// Asks on the terminal when there is one; declines otherwise.
async fn ask_permission(label: &str, subject: &str) -> Decision {
    if !std::io::stdin().is_terminal() {
        eprintln!("  ⎿ declined: no terminal to ask for approval (use --yes)");
        return Decision::Deny;
    }
    let question = format!("Allow {label}({subject})? [y]es / [a]lways / [N]o: ");
    tokio::task::spawn_blocking(move || {
        eprint!("{question}");
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        match answer.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => Decision::Allow,
            "a" | "always" => Decision::AllowAlways,
            _ => Decision::Deny,
        }
    })
    .await
    .unwrap_or(Decision::Deny)
}
