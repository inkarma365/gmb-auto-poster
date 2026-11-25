mod browser;
mod config;
mod gbp;
mod queue;
mod state;

use anyhow::{Context, Result};
use browser::{Driver, Target};
use clap::{Parser, Subcommand};
use config::Config;
use gbp::{Flow, Outcome};
use state::{Gate, RunLock, State};
use std::path::PathBuf;
use std::process::ExitCode;
use tracing::{error, info, warn};

const EXIT_SESSION: u8 = 2;

#[derive(Parser)]
#[command(
    version,
    about = "Pubblica un post al giorno su Google Business Profile da una cartella"
)]
struct Cli {
    #[arg(short, long, default_value = "config.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Apre Chromium visibile per il login manuale (una tantum). Chiudi la finestra quando hai finito.
    Login,
    /// Verifica che la sessione Google sia ancora valida.
    Check,
    /// Mostra e valida il prossimo post in coda.
    Next,
    /// Pubblica il prossimo post rispettando i limiti giornalieri.
    Post {
        /// Compila tutto ma si ferma prima di "Pubblica".
        #[arg(long)]
        dry_run: bool,
        /// Ignora i limiti giornalieri (solo per test manuali).
        #[arg(long)]
        force: bool,
        /// Lascia il browser aperto alla fine (chiudi la finestra per uscire).
        #[arg(long)]
        keep_open: bool,
    },
    /// Apre la scheda e cerca un elemento, evidenziandolo: serve per tarare flow.toml.
    Probe {
        /// Testi da cercare (testo visibile, aria-label, placeholder, title).
        #[arg(short, long)]
        text: Vec<String>,
        #[arg(long)]
        css: Option<String>,
        #[arg(long)]
        exact: bool,
        #[arg(long)]
        allow_hidden: bool,
        /// Clicca l'elemento trovato.
        #[arg(long)]
        click: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,chromiumoxide=warn".into()),
        )
        .with_target(false)
        .init();
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            error!("{e:#}");
            notify(&format!("Errore: {e}"));
            ExitCode::FAILURE
        }
    }
}

fn notify(msg: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=gmb-poster", "Google Business Poster", msg])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let cfg = Config::load(&cli.config)?;
    match cli.cmd {
        Cmd::Next => next(&cfg),
        Cmd::Login => login(&cfg).await,
        Cmd::Check => check(&cfg).await,
        Cmd::Post {
            dry_run,
            force,
            keep_open,
        } => post(&cfg, dry_run, force, keep_open).await,
        Cmd::Probe {
            text,
            css,
            exact,
            allow_hidden,
            click,
        } => {
            let t = Target {
                texts: text,
                css,
                exact,
                frame_url: None,
                allow_hidden,
            };
            probe(&cfg, t, click).await
        }
    }
}

fn lock(cfg: &Config) -> Result<RunLock> {
    RunLock::acquire(&cfg.base_dir())?.context("un'altra istanza di gmb-poster è già in esecuzione")
}

fn next(cfg: &Config) -> Result<ExitCode> {
    let Some(dir) = queue::next_dir(&cfg.posts_dir)? else {
        println!("Coda vuota: {}", queue::queue_dir(&cfg.posts_dir).display());
        return Ok(ExitCode::SUCCESS);
    };
    let p = queue::load(&dir, &cfg.copy)?;
    println!(
        "Prossimo post: {}\nImmagine: {}\nCaratteri: {}\n---\n{}",
        p.id,
        p.image.display(),
        p.text.chars().count(),
        p.text
    );
    Ok(ExitCode::SUCCESS)
}

async fn login(cfg: &Config) -> Result<ExitCode> {
    let _l = lock(cfg)?;
    let s = browser::launch(cfg, true).await?;
    s.page.goto("https://accounts.google.com/").await?;
    println!(
        "Accedi a Google nella finestra di Chromium (anche 2FA), poi apri la tua scheda:\n  {}\n\
         Quando vedi il pannello della scheda, CHIUDI la finestra: la sessione resta salvata in {}",
        cfg.business_url,
        cfg.profile_dir.display()
    );
    s.wait_closed().await;
    println!("Profilo salvato. Verifica con: gmb-poster check");
    Ok(ExitCode::SUCCESS)
}

async fn check(cfg: &Config) -> Result<ExitCode> {
    let _l = lock(cfg)?;
    let flow = Flow::load(&cfg.flow_file)?;
    let s = browser::launch(cfg, false).await?;
    let d = Driver::new(&s.page, &cfg.human, cfg.window);
    let ok = gbp::open_and_check(&d, cfg, &flow).await;
    if !matches!(ok, Ok(true)) {
        d.snapshot(&cfg.screenshots_dir, "check").await;
    }
    s.close().await;
    match ok? {
        true => {
            println!("Sessione valida.");
            Ok(ExitCode::SUCCESS)
        }
        false => {
            notify("Sessione Google scaduta: esegui `gmb-poster login`");
            Ok(ExitCode::from(EXIT_SESSION))
        }
    }
}

async fn post(cfg: &Config, dry_run: bool, force: bool, keep_open: bool) -> Result<ExitCode> {
    let _l = lock(cfg)?;
    let flow = Flow::load(&cfg.flow_file)?;
    let mut st = State::load(&cfg.state_file)?;

    if !dry_run
        && !force
        && let Gate::Skip(why) = st.gate(&cfg.limits)
    {
        info!("nessuna pubblicazione: {why}");
        return Ok(ExitCode::SUCCESS);
    }

    let Some(dir) = queue::next_dir(&cfg.posts_dir)? else {
        warn!("coda vuota");
        notify("Coda post vuota: aggiungi cartelle in posts/queue");
        return Ok(ExitCode::SUCCESS);
    };
    let p = match queue::load(&dir, &cfg.copy) {
        Ok(p) => p,
        Err(e) => {
            if !dry_run {
                let to = queue::archive(&cfg.posts_dir, &dir, "failed")?;
                std::fs::write(to.join("ERRORE.txt"), format!("{e:#}\n"))?;
            }
            return Err(e.context("post non valido, spostato in failed/"));
        }
    };
    info!("post selezionato: {}", p.id);

    if !dry_run {
        st.record_attempt()?;
    }

    let s = browser::launch(cfg, keep_open).await?;
    let mut d = Driver::new(&s.page, &cfg.human, cfg.window);

    let result = async {
        if !gbp::open_and_check(&d, cfg, &flow).await? {
            return Ok(None);
        }
        gbp::run(&mut d, cfg, &flow, &p, dry_run).await.map(Some)
    }
    .await;

    let code = match result {
        Ok(None) => {
            d.snapshot(&cfg.screenshots_dir, "sessione-scaduta").await;
            notify("Sessione Google scaduta: esegui `gmb-poster login`");
            ExitCode::from(EXIT_SESSION)
        }
        Ok(Some(Outcome::DryRunStopped)) => {
            let shot = d.snapshot(&cfg.screenshots_dir, "dry-run").await;
            println!(
                "Dry-run completato. Screenshot: {}",
                shot.map(|p| p.display().to_string()).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        Ok(Some(Outcome::Published)) => {
            d.snapshot(&cfg.screenshots_dir, "pubblicato").await;
            st.record_success(&p.id)?;
            let to = queue::archive(&cfg.posts_dir, &p.dir, "published")?;
            std::fs::write(
                to.join("published.json"),
                serde_json::to_vec_pretty(&serde_json::json!({
                    "published_at": chrono::Local::now().to_rfc3339(),
                    "business_url": cfg.business_url,
                }))?,
            )?;
            info!("pubblicato {} → {}", p.id, to.display());
            notify(&format!("Post pubblicato: {}", p.id));
            ExitCode::SUCCESS
        }
        Err(e) => {
            let shot = d.snapshot(&cfg.screenshots_dir, "errore").await;
            error!("{e:#}");
            if let Some(s) = &shot {
                error!("screenshot: {}", s.display());
            }
            if !dry_run {
                let n = st.record_failure(&p.id)?;
                if n >= cfg.limits.max_failures_per_post {
                    let to = queue::archive(&cfg.posts_dir, &p.dir, "failed")?;
                    std::fs::write(to.join("ERRORE.txt"), format!("{e:#}\n"))?;
                    warn!("{} fallito {n} volte, spostato in {}", p.id, to.display());
                }
            }
            notify(&format!("Pubblicazione fallita: {e}"));
            ExitCode::FAILURE
        }
    };

    if keep_open {
        println!("Browser lasciato aperto: chiudi la finestra per terminare.");
        s.wait_closed().await;
    } else {
        s.close().await;
    }
    Ok(code)
}

async fn probe(cfg: &Config, t: Target, click: bool) -> Result<ExitCode> {
    let _l = lock(cfg)?;
    let flow = Flow::load(&cfg.flow_file)?;
    let s = browser::launch(cfg, true).await?;
    let mut d = Driver::new(&s.page, &cfg.human, cfg.window);
    if !gbp::open_and_check(&d, cfg, &flow).await? {
        println!("Sessione non valida: esegui prima `gmb-poster login`.");
    }
    match d.find(&t).await? {
        Some(f) => {
            println!("Trovato: {}\nFrame: {}", f.desc, f.frame_url);
            d.highlight(&f).await?;
            if click {
                d.click(&f).await?;
            }
        }
        None => println!("Nessun elemento corrisponde a {t:?}"),
    }
    println!("Chiudi la finestra per terminare.");
    s.wait_closed().await;
    Ok(ExitCode::SUCCESS)
}
