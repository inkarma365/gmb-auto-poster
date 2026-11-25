# gmb-poster

[![CI](https://github.com/inkarma365/gmb-auto-poster/actions/workflows/ci.yml/badge.svg)](https://github.com/inkarma365/gmb-auto-poster/actions/workflows/ci.yml)

Pubblica un post al giorno (testo + 1 immagine) sulla scheda Google Business Profile, pilotando un Chromium reale con sessione persistente. Basato su [chromiumoxide](https://github.com/mattsse/chromiumoxide) (CDP).

## Setup

```sh
cargo build --release
cp config.toml.example config.toml   # imposta business_url
./target/release/gmb-poster login     # login manuale una tantum, poi chiudi la finestra
./target/release/gmb-poster check     # exit 0 = sessione valida, 2 = scaduta
```

## Post in coda

Vedi `examples/001-esempio/`. Il contenuto di `posts/` non viene versionato.

```
posts/queue/001-promo/post.txt     # testo (max 1500 caratteri, footer incluso)
posts/queue/001-promo/foto.jpg     # una sola immagine JPG/PNG, ≥ 250x250, 10 KB – 5 MB
```

Si pubblica la prima cartella in ordine alfabetico. Dopo la pubblicazione la cartella passa in `posts/published/AAAA-MM-GG-<nome>`. I post non validi, o falliti `max_failures_per_post` volte, finiscono in `posts/failed/` con un file `ERRORE.txt`.

```sh
gmb-poster next                      # mostra e valida il prossimo post
gmb-poster post --dry-run --keep-open  # compila tutto, non pubblica, lascia la finestra aperta
gmb-poster post                      # pubblica (rispetta i limiti giornalieri)
```

## Tarare l'interfaccia

Il flusso sta in `flow.toml`. Quando Google cambia l'interfaccia, si corregge lì. Per provare un selettore sulla scheda reale:

```sh
gmb-poster probe -t "Aggiungi aggiornamento"          # evidenzia l'elemento in rosso
gmb-poster probe -t "Aggiungi aggiornamento" --click
```

In caso di errore, screenshot e HTML della pagina finiscono in `screenshots/`.

## Precauzioni anti-rilevamento e rate limit

- Chromium con interfaccia (non headless) e profilo persistente; user-agent e fingerprint sono quelli reali, senza spoofing.
- Niente argomenti di default di chromiumoxide (`--enable-automation`, lingua e viewport emulati); `--disable-blink-features=AutomationControlled` → `navigator.webdriver === false`.
- Le query DOM girano in un isolated world, invisibile agli script della pagina.
- Click reali via `Input.dispatchMouseEvent` dopo un movimento del mouse su una curva di Bézier, in un punto casuale dentro l'elemento.
- Digitazione carattere per carattere con ritmo irregolare e pause.
- Passaggio da google.com prima della scheda; attese casuali tra le azioni (`[human]`).
- Al massimo 1 post al giorno, 2 tentativi al giorno, almeno 30 minuti tra un tentativo e l'altro (`[limits]`). Il timer parte a un orario casuale tra le 9:30 e le 10:15.

## Timer giornaliero (systemd user)

```sh
cp deploy/gmb-poster.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now gmb-poster.timer
systemctl --user list-timers gmb-poster.timer
journalctl --user -u gmb-poster -n 50
```

Il servizio apre Chromium sul display `:0`, quindi serve la sessione grafica attiva. `Persistent=true` recupera l'esecuzione persa se il PC era spento all'orario previsto.
