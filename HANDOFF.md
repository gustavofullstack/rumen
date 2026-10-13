# RUMEN — HANDOFF

> Navegador "Chromium 2.0" em Rust para CachyOS/Arch. Nome: **RUMEN**. Pasta: `~/rumen`.

**Pedido:** "Recriar o Chromium em Rust para CachyOS / Arch Linux."

## Decisão de escopo (leia primeiro)

Reescrever o Chromium linha por linha é inviável: são ~35 milhões de linhas de C++ (Blink, V8,
Skia, rede, sandbox), mantidas por centenas de engenheiros do Google. Engenharia reversa do
binário também não ajuda, porque o código-fonte do Chromium é aberto.

O caminho realista e 100% Rust é este:

| Camada do Chromium | Equivalente no RUMEN |
|---|---|
| Blink (layout/CSS/DOM) | **Servo** (Rust, Mozilla/Linux Foundation) via o crate `servo = 0.7.0` |
| V8 (JavaScript) | SpiderMonkey via `mozjs` (C++, já incluso no Servo; é o único pedaço que não é Rust) |
| Skia/compositor | WebRender (Rust) |
| Rede/TLS | `hyper` + `rustls` (Rust) |
| Interface do Chrome (abas, omnibox) | `src/browser.rs` + `res/ui.html` (uma "WebUI" renderizada pelo próprio Servo, como o Chrome faz) |
| Janela/entrada | `winit` (Wayland nativo no Hyprland; X11 como fallback) |

## Motor próprio: `crates/rumen-engine`

O navegador **só** importa `rumen_engine`, nunca o `servo` diretamente. Esse crate é o **nosso motor**:
hoje ele delega para os componentes Rust do Servo e, a partir daí, substituímos subsistema por
subsistema sem mexer na UI. O roteiro "Chromium 2.0 em Rust":

| Fase | Subsistema | Estado |
|---|---|---|
| M0 | Fachada `rumen-engine` + navegador funcional + release pública | ✅ validado em 13/10 (example.com, AUR, ArchWiki renderizando; abas e omnibox ok) |
| M1 | Vendorizar o Servo em `crates/` (fork MPL-2.0) para editar o motor livremente | próximo |
| M2 | Rede própria (`rumen-net`: hyper + rustls, cache HTTP, cookies persistentes, bloqueador de anúncios) | planejado |
| M3 | Perfis, histórico e favoritos (SQLite) + sincronização opcional via servidor próprio (Hostinger/EasyPanel) | planejado |
| M4 | Multiprocesso com sandbox (seccomp/namespaces, igual ao Chromium) | planejado |
| M5 | Trocar SpiderMonkey (C++) por um motor JS em Rust (Boa/Nova) quando a compatibilidade permitir | pesquisa |

Por que não "decompilar o Chromium para Rust": o código do Chromium já é aberto (não há o que
descobrir por engenharia reversa), e uma tradução automática de 35 milhões de linhas de C++ não
compila nem roda. Um motor novo e útil sai mais rápido partindo de componentes Rust que já passam
nos testes da web (WPT).

## Estrutura

```
rumen/
├── Cargo.toml          # workspace: app rumen + crates/rumen-engine
├── crates/rumen-engine/  # NOSSO motor (fachada sobre Servo 0.7.0)
├── src/main.rs         # loop de eventos winit, criação da janela (app-id "rumen"), sessão
├── src/browser.rs      # abas, omnibox, roteamento de mouse/teclado, composição GL, atalhos
├── src/resources.rs    # páginas internas embutidas, gravadas em ~/.local/share/rumen/internal
├── src/keyutils.rs     # conversão de teclas winit → DOM (copiado do servoshell, MPL-2.0)
├── res/ui.html         # barra de abas + omnibox (HTML/CSS/JS)
├── res/newtab.html     # página de nova aba
└── packaging/          # PKGBUILD, .desktop, ícone, install-user.sh
```

### Como funciona a composição
- Uma `WindowRenderingContext` (OpenGL/EGL via surfman) para a janela.
- Duas `OffscreenRenderingContext` filhas: uma para a UI (76 px CSS no topo) e uma compartilhada
  por todas as abas (só a aba ativa fica `show()`; as outras `hide()`).
- A cada redraw: `paint()` em cada WebView e `blit` dos framebuffers para a janela.
- A UI fala com o Rust por `console.log("RUMEN:{json}")` → `show_console_message`; o Rust
  fala com a UI via `evaluate_javascript("rumen.update(...)")`.
- Os comandos vão para uma fila e rodam depois de `spin_event_loop()`, para não reentrar no Servo.

## Funcionalidades prontas
- Várias abas, nova aba, fechar aba (inclusive com o botão do meio), `target=_blank` e `window.open` abrem nova aba
- Omnibox: URL, domínio sem esquema (`archlinux.org`) ou busca no DuckDuckGo
- Voltar/avançar/recarregar/início; botões laterais do mouse (voltar/avançar)
- Zoom (Ctrl +/−/0, Ctrl+roda) com indicador de %
- Tela cheia (F11 e Fullscreen API)
- Atalhos: Ctrl+T, Ctrl+W, Ctrl+L/F6, Ctrl+R/F5, Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+PgUp/PgDn,
  Ctrl+1..9, Alt+←/→, Alt+Home, Ctrl+Q
- Restauração de sessão (`~/.local/share/rumen/session.json`)
- Perfil persistente (cookies, localStorage, IndexedDB, cache) em `~/.local/share/rumen/profile`
- Título da janela = título da página; cursor do mouse segue a página
- **Popups nativos** (`src/popup.rs` + `res/popup.html`): `<select>`, menu de contexto, `alert`/`confirm`/`prompt`.
  São um widget separado (WebView própria) desenhado sobre a página, como no Chromium. Nada é injetado no site.
- **Modos de navegação** (`src/mode.rs`):
  - Normal: perfil persistente e sessão restaurada.
  - **Privado** (`--private`, Ctrl+Shift+N): perfil em `$XDG_RUNTIME_DIR` (RAM), apagado ao sair, inclusive
    em SIGINT/SIGTERM. Restos de processos mortos são limpos na inicialização. UA genérico.
  - **Tor** (`--tor`, Ctrl+Shift+O): privado + o RUMEN inicia o próprio `tor` (HTTPTunnelPort 9481) e
    passa todo o tráfego por ele. Abre `.onion`. Sem Tor instalado, a rede fica bloqueada (proxy morto)
    para não vazar o IP. **Requer `sudo pacman -S tor`.**
- **Web3:** `ipfs://` e `ipns://` (gateway dweb.link com isolamento por subdomínio) e `.eth` (eth.limo).
- Tema escuro; app-id `rumen` para regras do Hyprland

## Compilar / rodar / instalar (CachyOS)

```bash
# já feito nesta máquina: rustup em ~/.cargo (sem sudo)
cd ~/rumen && cargo build --release
./target/release/rumen https://wiki.cachyos.org
./packaging/install-user.sh          # instala em ~/.local/bin sem sudo
# ou como pacote do sistema:
cd packaging && makepkg -si
```

Regra opcional do Hyprland (um workspace por app):
```
windowrulev2 = workspace 6, class:^(rumen)$
```

## O que ainda falta (backlog por prioridade)
1. **Seletor de arquivos e de cor**: hoje são cancelados. Usar o portal XDG (`ashpd`) para o seletor nativo.
2. **Downloads**: Servo 0.7 tem suporte limitado; interceptar via `load_web_resource`.
3. **Favicons** nas abas (`webview.favicon()` → PNG base64 → UI).
4. **Histórico/favoritos** com SQLite (`rusqlite`) + sugestões na omnibox.
6. **Multiprocesso/sandbox**: feature `multiprocess` + `run_content_process` (padrão: processo único).
7. **Extensões e DevTools**: o Servo expõe um servidor DevTools compatível com o Firefox (`devtools` pref).
8. **Compatibilidade de sites**: o Servo ainda não roda tudo (Google Docs, alguns apps React pesados).
   Esse é o limite real do motor, não do RUMEN. Acompanhe https://servo.org/blog.

## Notas desta máquina
- VAIO i7-7500U, 4 threads, 15 GB de RAM: a build release completa leva cerca de 1 a 2 h e esquenta
  a CPU (throttling térmico). Builds incrementais depois disso levam poucos minutos.
- Sem sudo neste ambiente: o rustup foi instalado em `~/.cargo`. O `makepkg -si` precisa de senha.

## Armadilhas já resolvidas
- **Ordem de cópia na tela:** copiar o framebuffer da UI *antes* do conteúdo. Na ordem inversa, a área
  da página fica transparente, mesmo com o conteúdo renderizado (verificado via `read_to_image`).
- **`ClientStorage` em `/tmp` falhava:** resolvido passando `Opts.config_dir` (perfil persistente).
- Binário release: cerca de 120 MB. Build completa de 1 a 2 h; incremental de 2 a 10 min.

## Distribuição — PUBLICADO em 13/10/2026
- Repositório público: https://github.com/gustavofullstack/rumen (MPL-2.0)
- Release v0.1.0: https://github.com/gustavofullstack/rumen/releases/tag/v0.1.0 (tar.gz de 45 MB + SHA256SUMS)
- Site: https://gustavofullstack.github.io/rumen/ (branch `gh-pages` = conteúdo de `site/`)
- Instalação de um comando, testada em um HOME limpo: `curl -fsSL https://gustavofullstack.github.io/rumen/install.sh | sh`
- Domínio `rumen-browser.pages.dev` (Cloudflare Pages) reservado no plano. Para ativar, o dono roda
  `npx wrangler login` uma vez e depois `npx wrangler pages deploy site --project-name rumen-browser`.
- Nova versão: `cargo build --release -j2` → `scripts/release.sh` → `gh release create vX.Y.Z dist/*`
  → `git subtree split --prefix site -b gh-pages && git push -f origin gh-pages`.
- Logo atual: SVG vetorial (`packaging/rumen.svg`). A versão via Gemini/Nano Banana está agendada para quando
  a cota do Antigravity resetar.
- Planos: navegador gratuito para sempre. Serviços pagos (IA, hospedagem, sincronização) virão à parte.

## Ideias pedidas, ainda não implementadas
- Comunicação criptografada integrada (ex.: Matrix/Olm ou Signal Protocol em Rust via `vodozemac`).
- Builds para Windows/macOS/Android (Servo suporta; exige CI com runners de cada SO).
- Carteira Web3 / `window.ethereum` (exige muito cuidado de segurança).
