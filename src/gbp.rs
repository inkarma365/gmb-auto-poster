//! Flusso UI di Google Business Profile, descritto in flow.toml.

use crate::browser::{Driver, Target, pause};
use crate::config::Config;
use crate::queue::Post;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Deserialize)]
pub struct Flow {
    #[serde(default)]
    pub session: SessionCheck,
    #[serde(rename = "step", default)]
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
pub struct SessionCheck {
    /// Se l'URL contiene una di queste stringhe, la sessione è scaduta.
    #[serde(default = "d_logged_out")]
    pub logged_out_url: Vec<String>,
    /// Elemento presente solo da loggati sul pannello della scheda.
    pub logged_in: Option<Target>,
}

impl Default for SessionCheck {
    fn default() -> Self {
        Self {
            logged_out_url: d_logged_out(),
            logged_in: None,
        }
    }
}

fn d_logged_out() -> Vec<String> {
    vec![
        "accounts.google.com".into(),
        "/signin".into(),
        "ServiceLogin".into(),
    ]
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Goto,
    Click,
    Type,
    Upload,
    Wait,
    WaitGone,
    Pause,
    Screenshot,
}

#[derive(Debug, Deserialize)]
pub struct Step {
    pub name: String,
    pub action: Action,
    #[serde(flatten)]
    pub target: Target,
    /// Per `type` (default "{text}") e `goto` (default "{business_url}").
    pub value: Option<String>,
    /// Per `pause`: intervallo [min, max] in ms.
    pub ms: Option<[u64; 2]>,
    pub timeout_s: Option<u64>,
    /// Step di pubblicazione: in --dry-run il flusso si ferma prima di questo.
    #[serde(default)]
    pub publish: bool,
    /// Se fallisce, il flusso prosegue.
    #[serde(default)]
    pub optional: bool,
}

pub enum Outcome {
    Published,
    DryRunStopped,
}

impl Flow {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("lettura di {}", path.display()))?;
        let flow: Flow =
            toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        if flow.steps.is_empty() {
            bail!("{}: nessuno [[step]] definito", path.display());
        }
        if !flow.steps.iter().any(|s| s.publish) {
            bail!(
                "{}: nessuno step con publish = true (serve per --dry-run)",
                path.display()
            );
        }
        Ok(flow)
    }
}

/// Apre la scheda (passando prima dalla home di Google) e verifica di essere loggati.
pub async fn open_and_check(d: &Driver<'_>, cfg: &Config, flow: &Flow) -> Result<bool> {
    if let Some(w) = &cfg.warmup_url {
        d.goto(w).await?;
    }
    d.goto(&cfg.business_url).await?;
    let url = d.url().await;
    if flow
        .session
        .logged_out_url
        .iter()
        .any(|p| url.contains(p.as_str()))
    {
        warn!("sessione scaduta: reindirizzato a {url}");
        return Ok(false);
    }
    if let Some(t) = &flow.session.logged_in {
        if d.wait_for(t, None).await.is_err() {
            warn!("sessione: elemento di controllo non trovato su {url}");
            return Ok(false);
        }
    }
    info!("sessione valida ({url})");
    Ok(true)
}

fn subst(s: &str, cfg: &Config, post: &Post) -> String {
    s.replace("{text}", &post.text)
        .replace("{image}", &post.image.to_string_lossy())
        .replace("{business_url}", &cfg.business_url)
}

pub async fn run(
    d: &mut Driver<'_>,
    cfg: &Config,
    flow: &Flow,
    post: &Post,
    dry_run: bool,
) -> Result<Outcome> {
    let n = flow.steps.len();
    for (i, step) in flow.steps.iter().enumerate() {
        if step.publish && dry_run {
            info!("dry-run: mi fermo prima di \"{}\"", step.name);
            return Ok(Outcome::DryRunStopped);
        }
        info!("[{}/{n}] {}", i + 1, step.name);
        match run_step(d, cfg, step, post).await {
            Ok(()) => {}
            Err(e) if step.optional => warn!("step opzionale \"{}\" saltato: {e:#}", step.name),
            Err(e) => return Err(e.context(format!("step \"{}\"", step.name))),
        }
        if !matches!(step.action, Action::Pause) {
            pause(d.human.step_delay_ms).await;
        }
    }
    Ok(Outcome::Published)
}

async fn run_step(d: &mut Driver<'_>, cfg: &Config, step: &Step, post: &Post) -> Result<()> {
    let timeout = step.timeout_s.map(Duration::from_secs);
    match step.action {
        Action::Goto => {
            let url = subst(step.value.as_deref().unwrap_or("{business_url}"), cfg, post);
            d.goto(&url).await
        }
        Action::Click => {
            let f = d.wait_for(&step.target, timeout).await?;
            d.click(&f).await
        }
        Action::Type => {
            if !step.target.is_empty() {
                let f = d.wait_for(&step.target, timeout).await?;
                d.click(&f).await?;
                pause([300, 900]).await;
            }
            d.type_text(&subst(step.value.as_deref().unwrap_or("{text}"), cfg, post))
                .await
        }
        Action::Upload => {
            let f = d.wait_for(&step.target, timeout).await?;
            d.upload(&f, &post.image).await
        }
        Action::Wait => d.wait_for(&step.target, timeout).await.map(|_| ()),
        Action::WaitGone => d.wait_gone(&step.target, timeout).await,
        Action::Pause => {
            pause(step.ms.unwrap_or(d.human.step_delay_ms)).await;
            Ok(())
        }
        Action::Screenshot => {
            d.snapshot(&cfg.screenshots_dir, &step.name.replace(' ', "_"))
                .await;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_parses_and_has_publish_step() {
        let flow = Flow::load(Path::new("flow.toml")).unwrap();
        assert!(flow.session.logged_in.is_some());
        let publish: Vec<_> = flow.steps.iter().filter(|s| s.publish).collect();
        assert_eq!(publish.len(), 1);
        assert_eq!(publish[0].action, Action::Click);
    }
}
