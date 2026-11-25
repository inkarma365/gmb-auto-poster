use crate::config::Limits;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local, NaiveDate};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub days: BTreeMap<NaiveDate, Day>,
    /// Fallimenti per id del post.
    #[serde(default)]
    pub failures: BTreeMap<String, u32>,
    pub last_attempt: Option<DateTime<Local>>,
    #[serde(skip)]
    path: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Day {
    pub attempts: u32,
    pub published: Vec<String>,
}

pub enum Gate {
    Go,
    Skip(String),
}

impl State {
    pub fn load(path: &Path) -> Result<Self> {
        let mut st: State = match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e.into()),
        };
        st.path = path.to_path_buf();
        Ok(st)
    }

    pub fn save(&mut self) -> Result<()> {
        let cutoff = Local::now().date_naive() - Duration::days(90);
        self.days.retain(|d, _| *d >= cutoff);
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    fn today(&mut self) -> &mut Day {
        self.days.entry(Local::now().date_naive()).or_default()
    }

    /// Rate limit: un post al giorno, pochi tentativi, mai a raffica.
    pub fn gate(&mut self, l: &Limits) -> Gate {
        let day = self.today();
        if day.published.len() as u32 >= l.max_posts_per_day {
            return Gate::Skip(format!("già pubblicati {} post oggi", day.published.len()));
        }
        if day.attempts >= l.max_attempts_per_day {
            return Gate::Skip(format!("raggiunto il limite di {} tentativi oggi", day.attempts));
        }
        if let Some(last) = self.last_attempt {
            let wait = last + Duration::minutes(l.min_minutes_between_attempts) - Local::now();
            if wait > Duration::zero() {
                return Gate::Skip(format!("ultimo tentativo troppo recente, riprova tra {} min", wait.num_minutes() + 1));
            }
        }
        Gate::Go
    }

    pub fn record_attempt(&mut self) -> Result<()> {
        self.today().attempts += 1;
        self.last_attempt = Some(Local::now());
        self.save()
    }

    pub fn record_success(&mut self, id: &str) -> Result<()> {
        self.today().published.push(id.to_string());
        self.failures.remove(id);
        self.save()
    }

    /// Restituisce il numero di fallimenti aggiornato.
    pub fn record_failure(&mut self, id: &str) -> Result<u32> {
        let n = {
            let c = self.failures.entry(id.to_string()).or_default();
            *c += 1;
            *c
        };
        self.save()?;
        Ok(n)
    }
}

/// Lock esclusivo: evita due esecuzioni contemporanee sullo stesso profilo Chromium.
pub struct RunLock(#[allow(dead_code)] File);

impl RunLock {
    pub fn acquire(dir: &Path) -> Result<Option<Self>> {
        let f = File::create(dir.join("gmb-poster.lock"))?;
        match f.try_lock_exclusive() {
            Ok(()) => Ok(Some(RunLock(f))),
            Err(_) => Ok(None),
        }
    }
}
