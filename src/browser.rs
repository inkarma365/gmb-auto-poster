//! Avvio di Chromium con profilo persistente e azioni "umane" (mouse, tastiera, attese casuali).

use crate::config::{Config, Human};
use anyhow::{Context, Result, anyhow, bail};
use chromiumoxide::cdp::browser_protocol::dom::{GetContentQuadsParams, SetFileInputFilesParams};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    MouseButton,
};
use chromiumoxide::cdp::browser_protocol::page::{
    CreateIsolatedWorldParams, EventFileChooserOpened, SetInterceptFileChooserDialogParams,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    CallFunctionOnParams, EvaluateParams, RemoteObjectId,
};
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

pub struct Session {
    pub browser: Browser,
    pub page: Page,
    handler: JoinHandle<()>,
}

/// Avvia Chromium con il profilo persistente e senza i flag di automazione.
///
/// Gli argomenti di default di chromiumoxide (copiati da Puppeteer) includono `--enable-automation`,
/// `--lang=en_US`, `--password-store=basic` e un viewport emulato 800x600: li disattiviamo tutti.
/// Non usiamo `enable_stealth_mode()` perché imposta uno user-agent Windows/Chrome 107 che non
/// corrisponde ai client hints del Chromium reale. User-agent e fingerprint restano quelli veri.
pub async fn launch(cfg: &Config, headed: bool) -> Result<Session> {
    std::fs::create_dir_all(&cfg.profile_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cfg.profile_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let [w, h] = cfg.window;
    let mut b = BrowserConfig::builder()
        .chrome_executable(&cfg.chromium_path)
        .user_data_dir(cfg.profile_dir.canonicalize()?)
        .disable_default_args()
        .viewport(None)
        .window_size(w, h)
        .launch_timeout(Duration::from_secs(60))
        .request_timeout(Duration::from_secs(60))
        .args([
            // navigator.webdriver = false, niente banner "controllato da software automatizzato"
            ("disable-blink-features", "AutomationControlled"),
            ("lang", cfg.lang.as_str()),
            ("accept-lang", cfg.lang.as_str()),
        ])
        .args([
            "no-first-run",
            "no-default-browser-check",
            // Se la finestra resta dietro ad altre, Chromium non deve congelare timer e rendering.
            // Non sono visibili dalla pagina.
            "disable-backgrounding-occluded-windows",
            "disable-renderer-backgrounding",
            "disable-background-timer-throttling",
        ]);
    if cfg.no_sandbox {
        b = b.no_sandbox();
    }
    b = if headed || !cfg.headless {
        b.with_head()
    } else {
        b.new_headless_mode()
    };
    let conf = b
        .build()
        .map_err(|e| anyhow!("configurazione browser: {e}"))?;

    let (browser, mut handler) = Browser::launch(conf).await.context("avvio di Chromium")?;
    let handler = tokio::spawn(async move {
        while let Some(ev) = handler.next().await {
            if let Err(e) = ev {
                debug!("handler CDP: {e}");
            }
        }
    });
    let page = browser
        .new_page("about:blank")
        .await
        .context("apertura tab")?;
    // Chiude eventuali tab ripristinate dalla sessione precedente.
    if let Ok(pages) = browser.pages().await {
        for p in pages {
            if p.target_id() != page.target_id() {
                let _ = p.close().await;
            }
        }
    }
    Ok(Session {
        browser,
        page,
        handler,
    })
}

impl Session {
    /// Chiusura pulita: Chromium salva i cookie su disco.
    pub async fn close(mut self) {
        let _ = self.browser.close().await;
        let _ = tokio::time::timeout(Duration::from_secs(15), self.browser.wait()).await;
        self.handler.abort();
    }

    /// Attende che l'utente chiuda la finestra.
    pub async fn wait_closed(mut self) {
        let _ = self.browser.wait().await;
        self.handler.abort();
    }
}

/// Come trovare un elemento: per testo visibile / aria-label / placeholder / title, e/o CSS.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Target {
    #[serde(default)]
    pub texts: Vec<String>,
    pub css: Option<String>,
    #[serde(default)]
    pub exact: bool,
    /// Cerca solo nei frame il cui URL contiene questa stringa.
    pub frame_url: Option<String>,
    /// Accetta elementi invisibili (es. input[type=file]).
    #[serde(default)]
    pub allow_hidden: bool,
}

impl Target {
    pub fn is_empty(&self) -> bool {
        self.texts.is_empty() && self.css.is_none()
    }
}

pub struct Found {
    pub object_id: RemoteObjectId,
    pub frame_url: String,
    pub desc: String,
}

const FIND_JS: &str = r#"(spec) => {
  const norm = s => (s || '').replace(/\s+/g, ' ').trim().toLowerCase();
  const visible = el => {
    const r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return false;
    const cs = getComputedStyle(el);
    return cs.visibility !== 'hidden' && cs.display !== 'none' && parseFloat(cs.opacity || '1') > 0.05;
  };
  const INTERACTIVE = 'button, a, [role=button], [role=menuitem], [role=link], [role=tab], [role=option], textarea, input, [contenteditable=true], [contenteditable=""], [role=textbox]';
  const sel = spec.css || (INTERACTIVE + ', span, div, label');
  const found = [];
  const walk = root => {
    root.querySelectorAll(sel).forEach(e => found.push(e));
    root.querySelectorAll('*').forEach(e => { if (e.shadowRoot) walk(e.shadowRoot); });
  };
  walk(document);
  const texts = (spec.texts || []).map(norm).filter(Boolean);
  let best = null, bestScore = 0, bestLen = Infinity;
  for (const el of found) {
    if (!spec.allow_hidden && !visible(el)) continue;
    if (!texts.length) return el;
    const own = ['aria-label', 'placeholder', 'data-placeholder', 'title', 'alt'].map(a => el.getAttribute(a));
    const lb = el.getAttribute('aria-labelledby');
    if (lb) lb.split(/\s+/).forEach(id => { const n = document.getElementById(id); if (n) own.push(n.textContent); });
    const tc = el.textContent || '';
    if (tc.length <= 400) own.push(el.innerText);
    if (typeof el.value === 'string') own.push(el.value);
    const cands = own.map(norm).filter(Boolean);
    const len = tc.length;
    const bonus = el.matches(INTERACTIVE) ? 0.5 : 0;
    for (const t of texts) for (const c of cands) {
      let score = 0;
      if (c === t) score = 2 + bonus;
      else if (!spec.exact && c.includes(t) && c.length <= t.length * 3 + 30) score = 1 + bonus;
      if (score > bestScore || (score && score === bestScore && len < bestLen)) { best = el; bestScore = score; bestLen = len; }
    }
  }
  return best;
}"#;

const DESCRIBE_JS: &str = r#"function() {
  const t = (this.getAttribute('aria-label') || this.innerText || this.getAttribute('placeholder') || '').replace(/\s+/g, ' ').trim();
  return this.tagName.toLowerCase() + (this.getAttribute('role') ? '[role=' + this.getAttribute('role') + ']' : '') + ' "' + t.slice(0, 60) + '"';
}"#;

const SCROLL_JS: &str = r#"function() {
  const r = this.getBoundingClientRect();
  if (r.top < 0 || r.left < 0 || r.bottom > innerHeight || r.right > innerWidth) this.scrollIntoView({block: 'center', inline: 'center'});
}"#;

const IS_FILE_INPUT_JS: &str =
    "function() { return this.tagName === 'INPUT' && this.type === 'file'; }";

pub fn rand_ms([a, b]: [u64; 2]) -> Duration {
    Duration::from_millis(if b > a { rand::random_range(a..=b) } else { a })
}

pub async fn pause(range: [u64; 2]) {
    tokio::time::sleep(rand_ms(range)).await;
}

pub struct Driver<'a> {
    pub page: &'a Page,
    pub human: &'a Human,
    mouse: (f64, f64),
}

impl<'a> Driver<'a> {
    pub fn new(page: &'a Page, human: &'a Human, window: [u32; 2]) -> Self {
        let x = rand::random_range(100.0..(window[0] as f64 / 2.0));
        let y = rand::random_range(100.0..(window[1] as f64 / 2.0));
        Self {
            page,
            human,
            mouse: (x, y),
        }
    }

    pub async fn goto(&self, url: &str) -> Result<()> {
        info!("apro {url}");
        self.page
            .goto(url)
            .await
            .with_context(|| format!("navigazione verso {url}"))?;
        pause(self.human.page_settle_ms).await;
        Ok(())
    }

    pub async fn url(&self) -> String {
        self.page.url().await.ok().flatten().unwrap_or_default()
    }

    async fn call(&self, oid: &RemoteObjectId, func: &str) -> Result<Option<serde_json::Value>> {
        let p = CallFunctionOnParams::builder()
            .object_id(oid.clone())
            .function_declaration(func)
            .return_by_value(true)
            .build()
            .map_err(|e| anyhow!(e))?;
        Ok(self.page.execute(p).await?.result.result.value)
    }

    /// Cerca in tutti i frame (anche gli iframe di business.google.com dentro la Ricerca)
    /// usando un "isolated world": gli script della pagina non vedono le nostre query.
    pub async fn find(&self, t: &Target) -> Result<Option<Found>> {
        let spec = serde_json::to_string(t)?;
        for frame in self.page.frames().await? {
            let furl = self
                .page
                .frame_url(frame.clone())
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            if t.frame_url.as_deref().is_some_and(|f| !furl.contains(f)) {
                continue;
            }
            let Ok(params) = CreateIsolatedWorldParams::builder()
                .frame_id(frame.clone())
                .world_name("w")
                .build()
            else {
                continue;
            };
            let Ok(world) = self.page.execute(params).await else {
                continue;
            };
            let ev = EvaluateParams::builder()
                .expression(format!("({FIND_JS})({spec})"))
                .context_id(world.result.execution_context_id)
                .return_by_value(false)
                .build()
                .map_err(|e| anyhow!(e))?;
            let Ok(res) = self.page.execute(ev).await else {
                continue;
            };
            if let Some(oid) = res.result.result.object_id.clone() {
                let desc = self
                    .call(&oid, DESCRIBE_JS)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default();
                return Ok(Some(Found {
                    object_id: oid,
                    frame_url: furl,
                    desc,
                }));
            }
        }
        Ok(None)
    }

    pub async fn wait_for(&self, t: &Target, timeout: Option<Duration>) -> Result<Found> {
        let timeout = timeout.unwrap_or(Duration::from_secs(self.human.element_timeout_s));
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(f) = self.find(t).await? {
                return Ok(f);
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("elemento non trovato entro {}s: {:?}", timeout.as_secs(), t);
            }
            pause([400, 900]).await;
        }
    }

    pub async fn wait_gone(&self, t: &Target, timeout: Option<Duration>) -> Result<()> {
        let timeout = timeout.unwrap_or(Duration::from_secs(self.human.element_timeout_s));
        let deadline = tokio::time::Instant::now() + timeout;
        while self.find(t).await?.is_some() {
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "l'elemento è ancora presente dopo {}s: {:?}",
                    timeout.as_secs(),
                    t
                );
            }
            pause([400, 900]).await;
        }
        Ok(())
    }

    /// Punto da cliccare (coordinate del frame principale), con un po' di casualità attorno al centro.
    async fn click_point(&self, oid: &RemoteObjectId) -> Result<(f64, f64)> {
        self.call(oid, SCROLL_JS).await?;
        pause([250, 600]).await;
        let q = self
            .page
            .execute(
                GetContentQuadsParams::builder()
                    .object_id(oid.clone())
                    .build(),
            )
            .await
            .context("posizione dell'elemento")?;
        let quad = q
            .result
            .quads
            .first()
            .context("elemento senza area cliccabile")?;
        let v = quad.inner();
        let xs = [v[0], v[2], v[4], v[6]];
        let ys = [v[1], v[3], v[5], v[7]];
        let (minx, maxx) = (
            xs.iter().cloned().fold(f64::MAX, f64::min),
            xs.iter().cloned().fold(f64::MIN, f64::max),
        );
        let (miny, maxy) = (
            ys.iter().cloned().fold(f64::MAX, f64::min),
            ys.iter().cloned().fold(f64::MIN, f64::max),
        );
        let (cx, cy) = ((minx + maxx) / 2.0, (miny + maxy) / 2.0);
        let jx = (maxx - minx) * 0.2;
        let jy = (maxy - miny) * 0.2;
        let x = cx
            + if jx > 0.5 {
                rand::random_range(-jx..jx)
            } else {
                0.0
            };
        let y = cy
            + if jy > 0.5 {
                rand::random_range(-jy..jy)
            } else {
                0.0
            };
        Ok((x, y))
    }

    async fn mouse(&self, kind: DispatchMouseEventType, x: f64, y: f64) -> Result<()> {
        let mut b = DispatchMouseEventParams::builder()
            .r#type(kind.clone())
            .x(x)
            .y(y);
        if kind != DispatchMouseEventType::MouseMoved {
            b = b.button(MouseButton::Left).click_count(1);
        }
        self.page
            .execute(b.build().map_err(|e| anyhow!(e))?)
            .await?;
        Ok(())
    }

    /// Movimento lungo una curva di Bézier con velocità variabile, non un salto istantaneo.
    async fn move_to(&mut self, x: f64, y: f64) -> Result<()> {
        let (sx, sy) = self.mouse;
        let dist = ((x - sx).powi(2) + (y - sy).powi(2)).sqrt();
        let steps = (dist / 25.0).clamp(8.0, 40.0) as usize + rand::random_range(0..6usize);
        let off = (dist * 0.25).max(5.0);
        let (cx, cy) = (
            (sx + x) / 2.0 + rand::random_range(-off..off),
            (sy + y) / 2.0 + rand::random_range(-off..off),
        );
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let t = t * t * (3.0 - 2.0 * t); // ease-in-out
            let px = (1.0 - t).powi(2) * sx + 2.0 * (1.0 - t) * t * cx + t * t * x;
            let py = (1.0 - t).powi(2) * sy + 2.0 * (1.0 - t) * t * cy + t * t * y;
            self.mouse(DispatchMouseEventType::MouseMoved, px, py)
                .await?;
            pause([6, 22]).await;
        }
        self.mouse = (x, y);
        Ok(())
    }

    pub async fn click(&mut self, f: &Found) -> Result<()> {
        info!("clic su {}", f.desc);
        let (x, y) = self.click_point(&f.object_id).await?;
        self.move_to(x, y).await?;
        pause([80, 260]).await;
        self.mouse(DispatchMouseEventType::MousePressed, x, y)
            .await?;
        pause([55, 140]).await;
        self.mouse(DispatchMouseEventType::MouseReleased, x, y)
            .await?;
        Ok(())
    }

    async fn key(
        &self,
        kind: DispatchKeyEventType,
        key: &str,
        text: Option<&str>,
        code: Option<(&str, i64)>,
    ) -> Result<()> {
        let mut b = DispatchKeyEventParams::builder().r#type(kind).key(key);
        if let Some(t) = text {
            b = b.text(t).unmodified_text(t);
        }
        if let Some((c, vk)) = code {
            b = b
                .code(c)
                .windows_virtual_key_code(vk)
                .native_virtual_key_code(vk);
        }
        self.page
            .execute(b.build().map_err(|e| anyhow!(e))?)
            .await?;
        Ok(())
    }

    /// Digita un carattere alla volta con ritmo irregolare e pause dopo punteggiatura.
    pub async fn type_text(&self, text: &str) -> Result<()> {
        info!("digito {} caratteri", text.chars().count());
        let mut buf = [0u8; 4];
        for (i, ch) in text.chars().enumerate() {
            if ch == '\n' {
                self.key(
                    DispatchKeyEventType::KeyDown,
                    "Enter",
                    Some("\r"),
                    Some(("Enter", 13)),
                )
                .await?;
                self.key(
                    DispatchKeyEventType::KeyUp,
                    "Enter",
                    None,
                    Some(("Enter", 13)),
                )
                .await?;
            } else {
                let s = ch.encode_utf8(&mut buf);
                self.key(DispatchKeyEventType::KeyDown, s, Some(s), None)
                    .await?;
                self.key(DispatchKeyEventType::KeyUp, s, None, None).await?;
            }
            let mut d = rand_ms(self.human.type_delay_ms);
            if matches!(ch, ' ' | '.' | ',' | '!' | '?' | '\n') && rand::random_range(0..100) < 18 {
                d += rand_ms([180, 650]);
            }
            if i > 0 && i % 60 == 0 && rand::random_range(0..100) < 35 {
                d += rand_ms([700, 1800]);
            }
            tokio::time::sleep(d).await;
        }
        Ok(())
    }

    /// Carica un file: direttamente se il target è un input[type=file], altrimenti
    /// intercettando il selettore file che si apre cliccando il pulsante.
    pub async fn upload(&mut self, f: &Found, file: &Path) -> Result<()> {
        let path = file.to_string_lossy().to_string();
        let is_input = self
            .call(&f.object_id, IS_FILE_INPUT_JS)
            .await?
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_input {
            info!("carico {path} nell'input file");
            let p = SetFileInputFilesParams::builder()
                .files(vec![path])
                .object_id(f.object_id.clone())
                .build()
                .map_err(|e| anyhow!(e))?;
            self.page.execute(p).await?;
            return Ok(());
        }
        self.page
            .execute(SetInterceptFileChooserDialogParams::new(true))
            .await?;
        let mut events = self.page.event_listener::<EventFileChooserOpened>().await?;
        let res = async {
            self.click(f).await?;
            let ev = tokio::time::timeout(Duration::from_secs(20), events.next())
                .await
                .context("il selettore file non si è aperto")?
                .context("stream eventi chiuso")?;
            let node = ev.backend_node_id.context("selettore file senza nodo")?;
            info!("carico {path} dal selettore file");
            let p = SetFileInputFilesParams::builder()
                .files(vec![path.clone()])
                .backend_node_id(node)
                .build()
                .map_err(|e| anyhow!(e))?;
            self.page.execute(p).await?;
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = self
            .page
            .execute(SetInterceptFileChooserDialogParams::new(false))
            .await
        {
            warn!("disattivazione intercettazione file chooser: {e}");
        }
        res
    }

    pub async fn highlight(&self, f: &Found) -> Result<()> {
        self.call(
            &f.object_id,
            "function(){ this.style.outline = '3px solid #e11'; }",
        )
        .await?;
        Ok(())
    }

    /// Screenshot + HTML per capire cosa è andato storto.
    pub async fn snapshot(&self, dir: &Path, label: &str) -> Option<PathBuf> {
        std::fs::create_dir_all(dir).ok()?;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let png = dir.join(format!("{stamp}-{label}.png"));
        if let Err(e) = self
            .page
            .save_screenshot(ScreenshotParams::builder().build(), &png)
            .await
        {
            warn!("screenshot non riuscito: {e}");
        }
        if let Ok(html) = self.page.content().await {
            let _ = std::fs::write(dir.join(format!("{stamp}-{label}.html")), html);
        }
        Some(png)
    }
}
