# Projectstatus

## Release
Blueprint v2.6

## Huidige fase
Fase 2 gestart: handmatig portfolio (holdings) werkt op iOS + macOS, achter de device-login. Fase 0 + Fase 1 afgerond.

## Eerstvolgende taak
Live test van het Jarvis-brein: `JARVIS_LLM_API_KEY` (Anthropic) in de backend-`.env` zetten en het gesprek end-to-end verifiëren. Daarna IBKR live verifiëren zodra de Client Portal Gateway draait + ingelogd (paper eerst); vervolgens B — marktdata-provider (DEC-002) voor live koersen.

## Klaar
- Productvisie
- Architectuur
- Agents
- MCP/API-keuzes
- IBKR/MT5/futures/prop research
- NautilusTrader evaluatie
- Risk/backtest/securityprincipes
- Centrale TODO- en stappenstructuur
- JAR-001/002/003 gebouwd en geverifieerd: Rust workspace, Axum API (/livez, /readyz), typed config, tracing, SQLx-migraties, Docker dev-stack en CI
- Tauri 2 + Vue 3 client (Pinia + Vue Router) — `Jarvis.app` gebouwd en geverifieerd op macOS én iOS-simulator (iPhone 17, iOS 26.3)
- JAR-100 user/device model: `users`/`devices`/`device_keys`-schema + `jarvis-identity` repository (create/register/list/revoke), geverifieerd met unit- én Postgres-integratietests
- Client ↔ backend live: Tauri HTTP-plugin; Home-badge + Status-scherm halen live `/livez`+`/readyz` op — end-to-end geverifieerd op iOS-simulator ("Backend: verbonden")
- JAR-101 device-bound auth: Ed25519 challenge-response (`/v1/auth/challenge`+`/login`), sessies met gehashte tokens, Bearer-extractor + beschermde `/v1/devices`; HTTP-flow end-to-end getest
- Client-login werkend op iOS + macOS: keypair + signeren in Rust, dev auto-enroll + login, apparatenlijst via Bearer-token — geverifieerd met screenshot op iOS-simulator
- Fase 1-hardening: server-side logout + owner-checked device-revocatie; privésleutel in OS-keychain (`keyring`, Apple-backend) met app-privé fallback — iOS-build met keyring geverifieerd
- Fase 2 — portfolio: `holdings`-tabel (Decimal-geld) + `jarvis-portfolio` + beveiligde `/v1/holdings` (GET/POST/DELETE) met kostenbasis/allocatie; client Portfolio-scherm + Home-samenvatting. iOS geverifieerd (dashboard: "3 posities · kostenbasis 7842.75")
- IBKR read-only adapter (DEC-003 → ADR-013: Client Portal Web API): crate `jarvis-ibkr` (auth-status/accounts/positions + contracttests), beveiligde `/v1/broker/ibkr/status`+`/positions`, client IBKR-scherm. Live nog te testen met draaiende gateway.
- LLM-integratie (DEC-001 → ADR-022: Anthropic/Claude): crate `jarvis-llm` (`LlmProvider`-trait, Anthropic + Ollama-fallback, tiers Sonnet/Opus/Haiku), beveiligde `/v1/assistant/chat` met Nederlandse persona, client-chat op het echte brein. Sleutel alleen in backend-`.env`. Live test vereist `JARVIS_LLM_API_KEY`.

## Blockers/beslissingen
- Eerste marktdata-provider
- Praktische IBKR API-route
- Prop-firmkeuze
- Crypto versus prediction market
- Lokale hardware versus cloud-API

## Laatste update
9 augustus 2026

- **Server-side spraak — scaffolding (ADR-025)**: Picovoice viel af (signup weigert persoonlijke e-mail), dus stem gaat server-side. Nieuw crate `jarvis-speech` (`SpeechEngine`-trait + deterministische `StubEngine`, cosine-verificatie), migratie 0006 `voice_profiles` (één embedding per gebruiker → centraal = inherent gesynct naar devices), beveiligde endpoints `/v1/voice/status|enroll|verify`, config `JARVIS_SPEECH_*`. Keten getest met de stub (api-test enroll→verify). Echt model (Whisper/embeddings), client-opname/enroll-UI en lokaal wake-word = vervolgstappen. Client-voice-module staat op een inerte stub tot dat er is.
- **Brein-optimalisaties (goedkope winsten)**: system-prompt **prompt-caching** (Anthropic `cache_control` → goedkopere/snellere herhaalde calls), **token-usage** vastgelegd in `ChatReply` + gelogd (haak voor cost-tracking, Fase 1; Ollama mapt prompt/eval-counts), en een client **history-cap** (laatste 20 beurten) zodat lange chats de tokenkosten niet ongebrensd laten groeien. clippy/tests/build groen.
- **"Hey Jarvis" wake-word + stem-verificatie (ADR-024)**: on-device via Picovoice (Porcupine — "Jarvis" is ingebouwd — + Eagle speaker verification). Je neemt je stem één keer op (enrollment → profiel); alleen jouw stem activeert. Opt-in Settings-toggle (standaard uit), AccessKey + stemprofiel lokaal. **Stem = gemak, biometrie blijft slot**: ontgrendeld onthult "Hey Jarvis" de console + start de mic; vergrendeld start het alleen de Touch ID-prompt (bypasst het slot nooit). Nieuw: `voicewake.ts`, Settings-paneel, `wakePulse` → `JarvisConsole`. Dynamisch geïmporteerd/feature-flagged (app onaangetast als uit). Modellen via `npm run fetch-models` (niet in git). Client build groen. Live test vereist een gratis Picovoice AccessKey.
- **Lock dev-fix**: eenmaal ontgrendeld blijft de app ontgrendeld binnen dezelfde vensters-sessie (`sessionStorage`), zodat een hot-reload/reload niet opnieuw om Touch ID vraagt; een verse launch vergrendelt wél.
- **App-vergrendeling + ontgrendelen via telefoon (ADR-023)**: opt-in slot (Settings-toggle, standaard uit) op de desktop. Stap 1 = lokale biometrie (Touch ID/Face ID, biometrics-only) via native `biometric_unlock` (robius-authentication). Stap 2 als de biometrie faalt = **goedkeuren via de telefoon**: de desktop maakt een `unlock_request` (nonce) en pollt; de telefoon doet lokale biometrie (mét wachtwoord-fallback) en **tekent de nonce met zijn device-sleutel** (dezelfde Ed25519 als login), backend verifieert → `approved` → desktop ontgrendelt. Nieuwe migratie 0005 (`unlock_requests`) + `jarvis-identity`-functies + 4 beveiligde endpoints (`/v1/auth/unlock/*`). Client: `lock.ts`, `unlockApprovals.ts`, `AppLock.vue` (lockscreen), `UnlockApprovals.vue` (telefoon-overlay), Settings-toggle; `NSFaceIDUsageDescription` toegevoegd. Backend clippy/tests groen (identity 6 incl. unlock-flow, api 3), client build groen.
- **Cinematische Jarvis-homepage**: de reactor-core vult ~80% van het scherm als levende achtergrond; de conversatie zweeft eroverheen; de invoer schuift alleen omhoog bij hover linksonder (altijd zichtbaar op touch). Panelen/telemetrie van Home verwijderd (System/Trading bezitten die data). Mic/VAD naar composable `useMic`; oude `AssistantChat` vervangen.
- **Jarvis-brein bedraad (DEC-001 = Claude, ADR-022)**: nieuwe crate `crates/llm` met een provider-abstractie (`LlmProvider`-trait, `Arc<dyn>` in `AppState`). Providers: `AnthropicProvider` (raw HTTP → `POST /v1/messages`, tiers `claude-sonnet-5`/`claude-opus-5`/`claude-haiku-4-5`), `OllamaProvider` (lokale fallback `llama3.2`), `FallbackProvider` (valt terug bij transport-/API-fout, **niet** bij een echte `refusal`), plus `Unconfigured` + `Echo`-stub voor tests. Beveiligd (Bearer) endpoint `POST /v1/assistant/chat`: persona/system-prompt (`JARVIS_SYSTEM`, NL) wordt server-side toegevoegd, client stuurt alleen de beurten. Config `JARVIS_LLM_*` (sleutel geredigeerd in `Debug`, alleen backend). Client `assistant.ts` roept nu het echte endpoint aan met een "denkt na"-indicator; TTS spreekt alleen als de policy het toelaat. `cargo build`/`clippy -D warnings`/`test` (6 llm + config + API-integratie incl. chat-echo) en client `npm run build` groen. Nog te doen: SSE-streaming; live test met echte sleutel.
- Jarvis-HUD Home: levend reactor-core-scherm (canvas-particles + draaiende SVG-ringen + radar-sweep) met telemetrie-panelen (systeemstatus, device-mesh, portfolio, IBKR-link, live engine-feed), klok + uptime. Groen als standaard-accent voor de hele client (`styles.css`). Nieuw component `components/ReactorCore.vue`; `Home.vue` herschreven en op echte backend-/login-/portfolio-/IBKR-data aangesloten. `vue-tsc` + `vite build` groen. Respecteert `prefers-reduced-motion`.
- Navigatie omgebouwd naar een zwevende **Liquid-Glass dock** onderaan (alle schermen; geen zijbalk meer), 5 tabs met iconen (Jarvis/Portfolio/IBKR/System/Settings), Jarvis als hoofdtab. Core toont nu **alleen de naam "Jarvis"** en schaalt mee met het scherm (full-screen HUD). Nieuwe `Settings`-view met werkende **accentkleur-switch** (groen standaard, `theme.ts`, gepersisteerd in localStorage; particles volgen de accentkleur). Nieuw `components/NavIcon.vue`. Glass-design op panelen + dock.
- Navigatie herzien naar **twee lagen**: bovenaan een globale modus-schakelaar **SYSTEM / TRADING** (groene chip actief) + klok; onderaan een Liquid-Glass **sub-tab-dock** die meebeweegt met de modus (SYSTEM → Jarvis/System/Settings, TRADING → Portfolio/IBKR). Content van niet-HUD-views gecentreerd (`margin: 0 auto`).
- **Trading-view** samengevoegd: handmatige holdings + IBKR read-only in één desk-view (`views/Trading.vue`; sub-tab via route `/trading` en `/trading/ibkr`) met samenvattings-tiles. Oude `Portfolio.vue`/`Broker.vue` verwijderd; `/portfolio`+`/broker` redirecten. `vue-tsc` + `vite build` groen.
- **Jarvis-gesprek** links op de SYSTEM/Jarvis-HUD (`components/AssistantChat.vue`): tekst- én spraakinvoer, antwoord in chat en — als de policy het toelaat — hardop (TTS via `speechSynthesis`). Kern = **spraak-uitvoer-policy** (`voice.ts`): de audio-route bepaalt of Jarvis praat (oortje ⇒ praat ook als je typt; open luidspreker ⇒ standaard stil), master-mute, `canSpeak()` continu geëvalueerd. Mic **licht op met je stemvolume** (WebAudio-analyser/VAD) en gaat **na 5 s stilte** automatisch uit. Chat-iconen als nette SVG's (`NavIcon`). Brain (antwoorden) nog placeholder tot **DEC-001**. Docs: `voice/CONVERSATION_AND_OUTPUT_POLICY.md`, `decisions/ADR-021`, incl. **speaker-verification** (alleen jouw stem; JAR-151) en native route-detectie (JAR-150). Nieuwe open keuze **DEC-009** (STT-provider).
- README uitgebreid met een "Lokaal opstarten"-quickstart (Postgres → API → client, endpoints, iOS-sim, IBKR, checks).

- JAR-001 geïmplementeerd: Rust workspace, Axum API (/livez, /readyz), typed config, tracing, SQLx-migraties, Docker dev-stack en CI. Build/clippy/fmt/test groen; /readyz getest tegen Postgres 17.
- Tauri 2 + Vue 3 client (`apps/client`, Pinia + Vue Router) gescaffold; macOS `Jarvis.app` gebouwd en geverifieerd. iOS-project gegenereerd; simulator-build (iOS 26.3, iPhone 17) draait en geverifieerd met screenshot. Fase 0 afgerond.
- JAR-100: identity-datamodel (`users`/`devices`/`device_keys`, migratie 0002) + `jarvis-identity`-crate (repository + tests); CI uitgebreid met Postgres-service voor `#[sqlx::test]`-integratietests. Build/clippy/fmt/test groen.
- Client↔backend live-koppeling via `@tauri-apps/plugin-http` (`src/api.ts`); Home toont "Backend: verbonden", Status-scherm toont `/livez`+`/readyz`. End-to-end geverifieerd op iOS-simulator tegen draaiende API + Postgres (screenshot).
- JAR-101: device-bound auth (migratie 0003 sessions/challenges) + Ed25519 challenge-response + sessies; API gerefactord naar lib+bin met `/v1/auth/*` en beschermde `/v1/devices` (Bearer-extractor). HTTP-flow-integratietest groen.
- Client-login bedraad: Rust-commands (device_info/auth_public_key/auth_sign/auth_save/auth_session/auth_logout), dev-enroll-endpoint, `auth.ts`-flow + Home-UI. Volledige device-bound login end-to-end op iOS-simulator geverifieerd (screenshot: "ingelogd" + apparaat).
- Fase 1 afgerond: revocatie-endpoints (`/v1/auth/logout`, `DELETE /v1/devices/{id}`) + client-logout server-side; privésleutel naar OS-keychain (`keyring`) met fallback. iOS-e2e opnieuw geverifieerd.
- Fase 2 gestart — portfolio: migratie 0004 (`holdings`, numeric-geld) + crate `jarvis-portfolio` + beveiligde `/v1/holdings`-endpoints (kostenbasis/allocatie); client Portfolio-view + Home-samenvatting. iOS-dashboard geverifieerd met geseede posities (kostenbasis 7842.75, exacte Decimal). Provider-vrij; live koersen = B.
- IBKR read-only: DEC-003 gekozen (ADR-013, Client Portal Web API). Crate `jarvis-ibkr` (reqwest-client + getypeerde auth-status/accounts/positions + contracttests), config `JARVIS_IBKR_GATEWAY_URL`, beveiligde `/v1/broker/ibkr/status`+`/positions`, client IBKR-scherm. Build/clippy/tests groen. Live verificatie vereist draaiende Client Portal Gateway + interactieve SSO/2FA-login (door gebruiker).
