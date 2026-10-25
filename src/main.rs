use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use iron::auth::{self, BrowserLogin};
use iron::cortex::{ChatMessage, CortexClient, Harness};
use iron::session::{Session, SessionStore, BASE_URL_ENV, TOKEN_ENV};
use iron::DEFAULT_BASE_URL;

#[derive(Parser)]
#[command(
    name = "iron",
    version,
    about = "Iron — an AI agent harness for Mindify Cortex"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in to Mindify Cortex.
    Login(LoginArgs),
    /// Sign out: revoke the session on Cortex and delete it locally.
    Logout,
    /// Show whether Iron is signed in, verifying the session with Cortex.
    Status,
    /// List gateway models available to this account.
    Models(WorkspaceArgs),
    /// Ask a model one question through the Cortex gateway.
    Ask {
        #[command(flatten)]
        workspace: WorkspaceArgs,
        /// Gateway model ID (see `iron models`).
        #[arg(long, short)]
        model: String,
        /// The question.
        #[arg(required = true, num_args = 1..)]
        prompt: Vec<String>,
    },
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
    /// IRON_PASSWORD).
    #[arg(long, conflicts_with = "token_stdin")]
    email: Option<String>,
    /// Read an existing Cortex session token from stdin.
    #[arg(long)]
    token_stdin: bool,
}

#[derive(Args)]
struct WorkspaceArgs {
    /// `personal` or an organization ID.
    #[arg(long, default_value = "personal")]
    workspace: String,
    /// Protocol the launch is registered as; Iron speaks OpenAI Chat Completions.
    #[arg(long, value_enum, default_value = "opencode")]
    harness: Harness,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let store = SessionStore::default_location()?;
    match cli.command {
        Command::Login(args) => login(&store, args).await,
        Command::Logout => logout(&store).await,
        Command::Status => status(&store).await,
        Command::Models(args) => {
            let client = signed_in(&store)?;
            let models = client.models(&args.workspace, args.harness).await?;
            if models.is_empty() {
                println!(
                    "No models in workspace {:?} support the {} harness.",
                    args.workspace,
                    args.harness.as_str()
                );
            }
            for model in models {
                let tools = if model.supports_tools {
                    "tools"
                } else {
                    "text"
                };
                println!(
                    "{:<40} {:<14} {}  ({tools})",
                    model.id, model.provider, model.display_name
                );
            }
            Ok(())
        }
        Command::Ask {
            workspace,
            model,
            prompt,
        } => {
            let client = signed_in(&store)?;
            ask(&client, &workspace, &model, prompt.join(" ")).await
        }
    }
}

async fn login(store: &SessionStore, args: LoginArgs) -> Result<()> {
    let session = if args.token_stdin {
        let mut token = String::new();
        std::io::stdin().lock().read_line(&mut token)?;
        auth::token_login(&args.base_url, token.trim()).await?
    } else if let Some(email) = args.email {
        let password = match std::env::var("IRON_PASSWORD") {
            Ok(password) => password,
            Err(_) => rpassword::prompt_password(format!("Password for {email}: "))?,
        };
        auth::password_login(&args.base_url, &email, &password).await?
    } else {
        let open_browser = !args.no_browser && std::io::stdout().is_terminal();
        auth::browser_login(BrowserLogin::new(&args.base_url), |prompt| {
            let url = prompt.authorize_url.as_str();
            if open_browser && open::that_detached(url).is_ok() {
                eprintln!("Opening your browser to sign in to Cortex…\nIf it did not open, visit:\n  {url}");
            } else {
                eprintln!("Open this URL to sign in to Cortex:\n  {url}");
            }
            eprintln!("Waiting for the browser…");
        })
        .await?
    };
    store.save(&session)?;
    println!(
        "Signed in to {} as {}.",
        session.base_url,
        describe_user(&session)
    );
    Ok(())
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
        eprintln!("note: {TOKEN_ENV} is set in the environment and still signs Iron in.");
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

/// Runs one gateway turn under a scoped launch token, heartbeating while the
/// model works and always revoking the token afterwards.
async fn ask(
    client: &CortexClient,
    args: &WorkspaceArgs,
    model: &str,
    prompt: String,
) -> Result<()> {
    let ticket = client
        .create_launch(&args.workspace, args.harness, model)
        .await
        .context("could not start a gateway launch")?;

    let heartbeat = {
        let client = client.clone();
        let launch_id = ticket.launch_id.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.tick().await;
            loop {
                interval.tick().await;
                let _ = client.heartbeat_launch(&launch_id).await;
            }
        })
    };
    let messages = [ChatMessage::user(prompt)];
    let result = tokio::select! {
        result = client.complete(&ticket, &messages) => result.map_err(anyhow::Error::from),
        _ = tokio::signal::ctrl_c() => Err(anyhow::anyhow!("interrupted")),
    };
    heartbeat.abort();

    let exit_code = if result.is_ok() { 0 } else { 1 };
    if let Err(error) = client.finish_launch(&ticket.launch_id, exit_code).await {
        eprintln!(
            "warning: could not close gateway launch {}: {error}",
            ticket.launch_id
        );
    }
    let answer = result?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{}", answer.trim_end())?;
    Ok(())
}

fn signed_in(store: &SessionStore) -> Result<CortexClient> {
    match store.load()? {
        Some(session) => Ok(CortexClient::new(session)?),
        None => bail!("not signed in; run `iron login`"),
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
