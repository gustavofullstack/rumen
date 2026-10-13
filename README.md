<p align="center"><img src="packaging/rumen.svg" width="120" alt="RUMEN"></p>

<h1 align="center">RUMEN</h1>
<p align="center"><b>O navegador escrito em Rust.</b> Livre, privado e open source.<br>
<a href="https://gustavofullstack.github.io/rumen/">gustavofullstack.github.io/rumen</a></p>

---

## Instalar (Linux x86_64)

```sh
curl -fsSL https://gustavofullstack.github.io/rumen/install.sh | sh
```

O instalador baixa a última release, confere o SHA-256 e instala em `~/.local/bin` (sem sudo).

**Arch / CachyOS:** `cd packaging && makepkg -si`

## Recursos

| | |
|---|---|
| **Motor próprio em Rust** | `crates/rumen-engine`: layout, CSS, GPU (WebRender) e rede (hyper + rustls) sobre componentes do Servo |
| **Navegação normal** | Cookies, cache e sessão salvos localmente. Os dados são seus. |
| **Navegação privada** | `Ctrl+Shift+N` ou `rumen --private`. Perfil em RAM, apagado ao fechar. |
| **Modo Tor** | `Ctrl+Shift+O` ou `rumen --tor`. Todo o tráfego passa pelo Tor, com acesso a `.onion`. Se o Tor não estiver disponível, a rede é bloqueada para não vazar o IP. Requer o pacote `tor`. |
| **Web3** | `ipfs://`, `ipns://` e domínios `.eth` direto na barra de endereço |
| **Interface estilo Chromium** | Abas, omnibox, atalhos (`Ctrl+T/W/L/Tab`, `Alt+←/→`, `F11`…), zoom |

## Compilar

```sh
rustup default stable
cargo build --release      # a primeira build leva de 30 min a 2 h
./target/release/rumen https://example.com
```

Dependências de sistema (Arch): `clang python gstreamer gst-plugins-base harfbuzz fontconfig libxkbcommon wayland mesa`

## Arquitetura

```
rumen (app)            winit + UI "WebUI" em HTML (res/ui.html, res/popup.html)
  └── rumen-engine     motor do RUMEN (fachada estável)
        └── Servo 0.7  Stylo, layout, WebRender, rede, SpiderMonkey
```

Roteiro e decisões técnicas: [HANDOFF.md](HANDOFF.md).

## Planos

O RUMEN é e continuará **gratuito e open source**. No futuro, serviços opcionais e pagos (IA,
hospedagem, sincronização) serão oferecidos à parte, sem nunca limitar o navegador.

## Licença

[MPL-2.0](LICENSE)
