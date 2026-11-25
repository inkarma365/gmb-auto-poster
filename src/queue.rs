use crate::config::Copy;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub const MAX_TEXT_CHARS: usize = 1500;
const MIN_IMAGE_BYTES: u64 = 10 * 1024;
const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
const MIN_IMAGE_SIDE: usize = 250;
const TEXT_FILES: [&str; 2] = ["post.txt", "post.md"];
const IMAGE_EXT: [&str; 3] = ["jpg", "jpeg", "png"];

#[derive(Debug)]
pub struct Post {
    pub id: String,
    pub dir: PathBuf,
    pub text: String,
    pub image: PathBuf,
}

pub fn queue_dir(posts: &Path) -> PathBuf {
    posts.join("queue")
}

/// Prima cartella in ordine alfabetico dentro posts/queue (nomi tipo 001-..., 002-...).
pub fn next_dir(posts: &Path) -> Result<Option<PathBuf>> {
    let q = queue_dir(posts);
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&q)
        .with_context(|| format!("lettura di {}", q.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !is_hidden(p))
        .collect();
    dirs.sort();
    Ok(dirs.into_iter().next())
}

fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

pub fn load(dir: &Path, copy: &Copy) -> Result<Post> {
    let id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .context("nome cartella non valido")?
        .to_string();

    let body = TEXT_FILES
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())
        .map(std::fs::read_to_string)
        .transpose()?
        .map(|s| s.trim().to_string())
        .or_else(|| copy.default_text.clone())
        .filter(|s| !s.is_empty());
    let Some(body) = body else {
        bail!("{id}: manca post.txt e non c'è copy.default_text in config");
    };
    let text = match copy
        .footer
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        Some(f) => format!("{body}\n\n{f}"),
        None => body,
    };
    let n = text.chars().count();
    if n > MAX_TEXT_CHARS {
        bail!("{id}: testo di {n} caratteri, il massimo è {MAX_TEXT_CHARS}");
    }

    let mut images: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
        })
        .collect();
    images.sort();
    let image = match images.len() {
        0 => bail!("{id}: nessuna immagine .jpg/.png"),
        1 => images.remove(0),
        n => bail!("{id}: trovate {n} immagini, ne serve una sola"),
    };
    let bytes = std::fs::metadata(&image)?.len();
    if !(MIN_IMAGE_BYTES..=MAX_IMAGE_BYTES).contains(&bytes) {
        bail!(
            "{id}: immagine di {} KB, deve stare tra 10 KB e 5 MB",
            bytes / 1024
        );
    }
    let sz = imagesize::size(&image).with_context(|| format!("{id}: immagine non leggibile"))?;
    if sz.width < MIN_IMAGE_SIDE || sz.height < MIN_IMAGE_SIDE {
        bail!(
            "{id}: immagine {}x{}, minimo {MIN_IMAGE_SIDE}x{MIN_IMAGE_SIDE}",
            sz.width,
            sz.height
        );
    }
    let image = image.canonicalize()?;
    Ok(Post {
        id,
        dir: dir.to_path_buf(),
        text,
        image,
    })
}

/// Sposta la cartella del post in posts/<dest>/<data>-<id> e restituisce il nuovo percorso.
pub fn archive(posts: &Path, dir: &Path, dest: &str) -> Result<PathBuf> {
    let name = dir
        .file_name()
        .context("cartella senza nome")?
        .to_string_lossy();
    let stamp = chrono::Local::now().format("%Y-%m-%d");
    let target_dir = posts.join(dest);
    std::fs::create_dir_all(&target_dir)?;
    let mut target = target_dir.join(format!("{stamp}-{name}"));
    let mut i = 2;
    while target.exists() {
        target = target_dir.join(format!("{stamp}-{name}-{i}"));
        i += 1;
    }
    std::fs::rename(dir, &target)?;
    Ok(target)
}
