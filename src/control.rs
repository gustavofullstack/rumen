/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Canal de controle local do RUMEN: um socket Unix (`$XDG_RUNTIME_DIR/rumen.sock`, permissão
//! 0600, só o próprio usuário acessa) que recebe comandos JSON, um por linha. É por ele que
//! ferramentas (o servidor MCP `rumen mcp`, scripts, agentes) operam o navegador **aberto**:
//! abrir abas, ler páginas, rodar JS, recarregar a interface, tudo sem fechar a janela.
//!
//! Protocolo: `{"cmd":"tabs"}\n` → `{"ok":true,"result":...}\n`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use rumen_engine::{EventLoopWaker, JSValue};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, Debug)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ControlCmd {
    /// Versão e modo da instância.
    Ping,
    /// Lista as abas (índice, título, URL, carregando, ativa).
    Tabs,
    /// Abre uma URL (ou busca) na aba ativa ou numa nova aba.
    Open {
        url: String,
        #[serde(default)]
        new_tab: bool,
    },
    Select {
        index: usize,
    },
    Close {
        index: usize,
    },
    Back,
    Forward,
    Reload,
    /// Executa JavaScript na aba indicada (padrão: ativa) e devolve o resultado.
    Eval {
        js: String,
        #[serde(default)]
        index: Option<usize>,
    },
    /// Texto visível da página (`document.body.innerText`).
    Text {
        #[serde(default)]
        index: Option<usize>,
    },
    /// Recarrega a interface do navegador ao vivo. Com `dir`, copia antes
    /// `ui.html`/`newtab.html`/`popup.html` desse diretório (ex.: `~/rumen/res`).
    ReloadUi {
        #[serde(default)]
        dir: Option<String>,
    },
}

pub struct Request {
    pub cmd: ControlCmd,
    pub reply: Sender<Value>,
}

impl Request {
    pub fn ok(&self, result: Value) {
        let _ = self.reply.send(json!({ "ok": true, "result": result }));
    }

    pub fn err(&self, message: impl Into<String>) {
        let _ = self.reply.send(json!({ "ok": false, "error": message.into() }));
    }
}

pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("RUMEN_SOCKET") {
        return PathBuf::from(p);
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("rumen.sock")
}

/// Começa a escutar. Devolve `None` se outra instância do RUMEN já controla o socket.
pub fn listen(waker: Box<dyn EventLoopWaker>) -> Option<Receiver<Request>> {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        return None; // outra instância viva
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).ok()?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("rumen-control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                let waker = waker.clone_box();
                std::thread::spawn(move || serve(stream, tx, waker));
            }
        })
        .ok()?;
    Some(rx)
}

fn serve(stream: UnixStream, tx: Sender<Request>, waker: Box<dyn EventLoopWaker>) {
    let Ok(mut writer) = stream.try_clone() else { return };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<ControlCmd>(&line) {
            Ok(cmd) => {
                let (reply, wait) = channel();
                if tx.send(Request { cmd, reply }).is_err() {
                    break;
                }
                waker.wake();
                wait.recv_timeout(Duration::from_secs(30))
                    .unwrap_or_else(|_| json!({ "ok": false, "error": "tempo esgotado" }))
            },
            Err(e) => json!({ "ok": false, "error": format!("comando inválido: {e}") }),
        };
        if writeln!(writer, "{response}").is_err() {
            break;
        }
    }
}

/// Envia um comando a uma instância em execução (usado por `rumen mcp` e `rumen ctl`).
pub fn send(cmd: &Value) -> std::io::Result<Value> {
    let mut stream = UnixStream::connect(socket_path())?;
    stream.set_read_timeout(Some(Duration::from_secs(35)))?;
    writeln!(stream, "{cmd}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|e| std::io::Error::other(e.to_string()))
}

pub fn js_to_json(value: JSValue) -> Value {
    match value {
        JSValue::Undefined | JSValue::Null => Value::Null,
        JSValue::Boolean(b) => Value::Bool(b),
        JSValue::Number(n) => json!(n),
        JSValue::String(s) => Value::String(s),
        JSValue::Element(id) => json!({ "element": id }),
        JSValue::ShadowRoot(id) => json!({ "shadow_root": id }),
        JSValue::Frame(id) => json!({ "frame": id }),
        JSValue::Window(id) => json!({ "window": id }),
        JSValue::Array(items) => Value::Array(items.into_iter().map(js_to_json).collect()),
        JSValue::Object(map) => {
            Value::Object(map.into_iter().map(|(k, v)| (k, js_to_json(v))).collect())
        },
    }
}
