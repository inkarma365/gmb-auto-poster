use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// URL del pannello della scheda (Ricerca Google "La mia attività" o business.google.com).
    pub business_url: String,
    /// Pagina "di passaggio" aperta prima della scheda, come farebbe una persona.
    #[serde(default = "d_warmup")]
    pub warmup_url: Option<String>,
    #[serde(default = "d_posts")]
    pub posts_dir: PathBuf,
    #[serde(default = "d_profile")]
    pub profile_dir: PathBuf,
    #[serde(default = "d_screens")]
    pub screenshots_dir: PathBuf,
    #[serde(default = "d_state")]
    pub state_file: PathBuf,
    #[serde(default = "d_flow")]
    pub flow_file: PathBuf,
    #[serde(default = "d_chromium")]
    pub chromium_path: PathBuf,
    /// Sconsigliato: Chromium headless è il segnale di automazione più facile da riconoscere.
    #[serde(default)]
    pub headless: bool,
    #[serde(default = "d_window")]
    pub window: [u32; 2],
    #[serde(default = "d_lang")]
    pub lang: String,
    #[serde(default)]
    pub copy: Copy,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub human: Human,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Copy {
    /// Testo usato quando la cartella del post non contiene post.txt.
    pub default_text: Option<String>,
    /// Testo aggiunto in fondo a ogni post (es. contatti, hashtag).
    pub footer: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    pub max_posts_per_day: u32,
    pub max_attempts_per_day: u32,
    pub min_minutes_between_attempts: i64,
    /// Dopo questi fallimenti il post viene spostato in failed/.
    pub max_failures_per_post: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_posts_per_day: 1,
            max_attempts_per_day: 2,
            min_minutes_between_attempts: 30,
            max_failures_per_post: 3,
        }
    }
}

/// Intervalli [min, max] in millisecondi: ogni attesa è estratta a caso nell'intervallo.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Human {
    pub step_delay_ms: [u64; 2],
    pub page_settle_ms: [u64; 2],
    pub type_delay_ms: [u64; 2],
    pub element_timeout_s: u64,
}

impl Default for Human {
    fn default() -> Self {
        Self {
            step_delay_ms: [1200, 3500],
            page_settle_ms: [2500, 6000],
            type_delay_ms: [45, 170],
            element_timeout_s: 30,
        }
    }
}

fn d_warmup() -> Option<String> {
    Some("https://www.google.com/".into())
}
fn d_posts() -> PathBuf {
    "posts".into()
}
fn d_profile() -> PathBuf {
    "chrome-profile".into()
}
fn d_screens() -> PathBuf {
    "screenshots".into()
}
fn d_state() -> PathBuf {
    "state.json".into()
}
fn d_flow() -> PathBuf {
    "flow.toml".into()
}
fn d_chromium() -> PathBuf {
    "/usr/bin/chromium".into()
}
fn d_window() -> [u32; 2] {
    [1440, 900]
}
fn d_lang() -> String {
    "it-IT".into()
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path).with_context(|| {
            format!("lettura di {} (copia config.toml.example)", path.display())
        })?;
        let mut cfg: Config =
            toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        let base = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        for p in [
            &mut cfg.posts_dir,
            &mut cfg.profile_dir,
            &mut cfg.screenshots_dir,
            &mut cfg.state_file,
            &mut cfg.flow_file,
        ] {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
        Ok(cfg)
    }

    pub fn base_dir(&self) -> PathBuf {
        self.state_file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let cfg = Config::load(Path::new("config.toml.example")).unwrap();
        assert_eq!(cfg.limits.max_posts_per_day, 1);
        assert!(!cfg.headless);
        assert!(cfg.human.type_delay_ms[0] <= cfg.human.type_delay_ms[1]);
    }
}
