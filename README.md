# pi agent desktop

App desktop per Windows e macOS che apre in una finestra dedicata il terminale web di
[pi](https://github.com/badlogic/pi-mono) servito da [ttyd](https://github.com/tsl0922/ttyd)
dietro un reverse proxy con Basic Auth.

## Perché un proxy locale

Le webview di sistema (WebView2 su Windows, WKWebView su macOS) non mostrano la richiesta
di credenziali Basic Auth, e le WebSocket del browser non possono mandare l'header
`Authorization`. L'app avvia quindi un piccolo proxy su `127.0.0.1` (porta casuale e
percorso segreto casuale) che inoltra pagina, `/token` e `/ws` al server aggiungendo le
credenziali. La finestra carica il proxy locale.

## Uso

1. Scarica l'installer dall'[ultima release](../../releases/latest):
   - **Windows**: `pi.agent_x.y.z_x64-setup.exe`
   - **macOS** (Intel e Apple Silicon): `pi.agent_x.y.z_universal.dmg`. L'app non è firmata
     da Apple: al primo avvio clic destro → **Apri**.
2. Al primo avvio inserisci indirizzo del server, utente e password.
   La password resta nel portachiavi del sistema (Credential Manager / Keychain).
3. Menu **pi**: *Ricarica*, *Account…* (cambia server o utente), *Controlla aggiornamenti…*

L'app controlla gli aggiornamenti a ogni avvio e li installa dopo conferma.

## Server

Serve un ttyd raggiungibile in HTTPS, protetto da Basic Auth sul reverse proxy, con le
WebSocket abilitate. Esempio del lato server: `ttyd -i 127.0.0.1 -p 7681 -W tmux new -A -s pi pi`.

## Sviluppo

```bash
npm ci
npm run tauri dev      # finestra di sviluppo
npm run tauri build    # installer locale
```

Rilascio: aggiorna `version` in `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` e
`package.json`, poi `git tag vX.Y.Z && git push --tags`. Il workflow `release` compila per
Windows e macOS, firma gli aggiornamenti (secret `TAURI_SIGNING_PRIVATE_KEY` e
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`) e pubblica `latest.json`.
