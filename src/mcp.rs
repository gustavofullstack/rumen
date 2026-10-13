/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `rumen mcp`: servidor MCP (Model Context Protocol, JSON-RPC 2.0 via stdio) que expõe o
//! navegador em execução como ferramentas para agentes (Claude Code etc.). Cada chamada vira
//! um comando no socket de controle (`control.rs`). Se o RUMEN não estiver aberto, a
//! primeira chamada o abre.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::control;

const PROTOCOL_VERSION: &str = "2025-06-18";

fn tools() -> Value {
    let idx = json!({ "type": "integer", "description": "Índice da aba (padrão: aba ativa)" });
    json!([
        { "name": "rumen_tabs", "description": "Lista as abas abertas no RUMEN (índice, título, URL, se está carregando e qual está ativa).",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "rumen_open", "description": "Abre uma URL (ou texto de busca) no RUMEN, na aba ativa ou numa nova aba.",
          "inputSchema": { "type": "object", "properties": {
              "url": { "type": "string" }, "new_tab": { "type": "boolean", "default": true } }, "required": ["url"] } },
        { "name": "rumen_select_tab", "description": "Muda para a aba indicada.",
          "inputSchema": { "type": "object", "properties": { "index": { "type": "integer" } }, "required": ["index"] } },
        { "name": "rumen_close_tab", "description": "Fecha a aba indicada. O navegador nunca fecha: se era a última, abre uma nova aba.",
          "inputSchema": { "type": "object", "properties": { "index": { "type": "integer" } }, "required": ["index"] } },
        { "name": "rumen_navigate", "description": "Voltar, avançar ou recarregar a aba ativa.",
          "inputSchema": { "type": "object", "properties": {
              "action": { "type": "string", "enum": ["back", "forward", "reload"] } }, "required": ["action"] } },
        { "name": "rumen_page_text", "description": "Texto visível da página (document.body.innerText).",
          "inputSchema": { "type": "object", "properties": { "index": idx } } },
        { "name": "rumen_eval", "description": "Executa JavaScript na página e devolve o resultado (JSON).",
          "inputSchema": { "type": "object", "properties": { "js": { "type": "string" }, "index": idx }, "required": ["js"] } },
        { "name": "rumen_reload_ui", "description": "Recarrega a interface do RUMEN (abas, barra, popups) ao vivo, sem fechar a janela. Com 'dir', aplica antes os arquivos ui.html/newtab.html/popup.html desse diretório (ex.: /home/USER/rumen/res).",
          "inputSchema": { "type": "object", "properties": { "dir": { "type": "string" } } } },
        { "name": "rumen_status", "description": "Versão, modo e se o RUMEN está rodando.",
          "inputSchema": { "type": "object", "properties": {} } }
    ])
}

fn to_command(name: &str, args: &Value) -> Result<Value, String> {
    let get = |k: &str| args.get(k).cloned().unwrap_or(Value::Null);
    Ok(match name {
        "rumen_tabs" => json!({ "cmd": "tabs" }),
        "rumen_open" => json!({ "cmd": "open", "url": get("url"),
                                "new_tab": args.get("new_tab").and_then(Value::as_bool).unwrap_or(true) }),
        "rumen_select_tab" => json!({ "cmd": "select", "index": get("index") }),
        "rumen_close_tab" => json!({ "cmd": "close", "index": get("index") }),
        "rumen_navigate" => match args.get("action").and_then(Value::as_str) {
            Some(a @ ("back" | "forward" | "reload")) => json!({ "cmd": a }),
            _ => return Err("action deve ser back, forward ou reload".into()),
        },
        "rumen_page_text" => json!({ "cmd": "text", "index": get("index") }),
        "rumen_eval" => json!({ "cmd": "eval", "js": get("js"), "index": get("index") }),
        "rumen_reload_ui" => json!({ "cmd": "reload_ui", "dir": get("dir") }),
        "rumen_status" => json!({ "cmd": "ping" }),
        other => return Err(format!("ferramenta desconhecida: {other}")),
    })
}

/// Envia ao RUMEN aberto; se não houver instância, abre uma e espera o socket (até 20 s).
fn call(cmd: &Value) -> Result<Value, String> {
    if let Ok(v) = control::send(cmd) {
        return Ok(v);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("não foi possível abrir o RUMEN: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        if let Ok(v) = control::send(cmd) {
            return Ok(v);
        }
    }
    Err("o RUMEN não respondeu (socket de controle indisponível)".into())
}

pub fn run() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = msg.get("id").cloned() else { continue }; // notificação: sem resposta
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").cloned()
                    .unwrap_or_else(|| json!(PROTOCOL_VERSION)),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "rumen", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Controla o navegador RUMEN aberto na máquina do usuário. Nunca feche o navegador; use as abas."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let outcome = to_command(name, &args).and_then(|cmd| call(&cmd));
                Ok(match outcome {
                    Ok(v) => {
                        let is_error = v.get("ok") == Some(&Value::Bool(false));
                        let body = v.get("result").cloned().unwrap_or(v.clone());
                        let text = match body {
                            Value::String(s) => s,
                            other => serde_json::to_string_pretty(&other).unwrap_or_default(),
                        };
                        let text = if is_error {
                            v.get("error").and_then(Value::as_str).unwrap_or("erro").to_owned()
                        } else {
                            text
                        };
                        json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
                    },
                    Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
                })
            },
            _ => Err((-32601, format!("método não suportado: {method}"))),
        };
        let response = match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            },
        };
        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }
}
