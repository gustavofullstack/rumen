/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Popups nativos do navegador (lista de `<select>`, menu de contexto, `alert`/`confirm`/`prompt`).
//!
//! Assim como no Chromium, o popup é um widget separado, uma WebView própria (`res/popup.html`)
//! desenhada por cima da página. Nada é injetado no site, e o conteúdo da página não consegue
//! se passar pela interface do navegador.

use rumen_engine::{
    ContextMenu, ContextMenuAction, ContextMenuItem, EmbedderControl, EmbedderControlId,
    SelectElement, SelectElementOptionOrOptgroup, SimpleDialog,
};
use serde_json::{Value, json};

pub enum PopupKind {
    Select(SelectElement),
    Menu(ContextMenu, Vec<ContextMenuAction>),
    Dialog(SimpleDialog),
}

/// Retângulo em pixels de dispositivo, relativo ao canto superior esquerdo da janela.
#[derive(Clone, Copy, Debug)]
pub struct WinRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl WinRect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x as f64 &&
            y >= self.y as f64 &&
            x < (self.x + self.w) as f64 &&
            y < (self.y + self.h) as f64
    }
}

pub struct Popup {
    pub id: EmbedderControlId,
    pub kind: PopupKind,
    pub rect: WinRect,
    pub payload: Value,
}

/// Geometria disponível para posicionar o popup.
pub struct Layout {
    pub scale: f32,
    pub ui_h: i32,
    pub win_w: i32,
    pub win_h: i32,
    pub origin: String,
}

const ROW: f32 = 28.0;
const GROUP: f32 = 24.0;
const SEP: f32 = 9.0;
const PAD: f32 = 10.0;

impl Popup {
    /// Converte o controle pedido pelo motor em popup. Devolve `None` para controles que o
    /// RUMEN ainda não apresenta (o motor os recebe como cancelados quando são descartados).
    pub fn from_control(control: EmbedderControl, layout: &Layout) -> Option<Self> {
        let id = control.id();
        let s = layout.scale;
        match control {
            EmbedderControl::SelectElement(select) => {
                let selected = select.selected_options();
                let mut entries = Vec::new();
                let mut height = PAD;
                let push_option = |o: &rumen_engine::SelectElementOption,
                                       grouped: bool,
                                       entries: &mut Vec<Value>| {
                    entries.push(json!({
                        "label": o.label, "value": o.id, "enabled": !o.is_disabled,
                        "selected": selected.contains(&o.id), "grouped": grouped,
                    }));
                };
                for item in select.options() {
                    match item {
                        SelectElementOptionOrOptgroup::Option(o) => {
                            push_option(o, false, &mut entries);
                            height += ROW;
                        },
                        SelectElementOptionOrOptgroup::Optgroup { label, options } => {
                            entries.push(json!({ "type": "group", "label": label }));
                            height += GROUP;
                            for o in options {
                                push_option(o, true, &mut entries);
                                height += ROW;
                            }
                        },
                    }
                }
                let anchor = select.position();
                let w = (anchor.width()).max((160.0 * s) as i32);
                let h = ((height.min(360.0)) * s) as i32;
                let below = anchor.max.y + layout.ui_h;
                let y = if below + h > layout.win_h {
                    (anchor.min.y + layout.ui_h - h).max(layout.ui_h)
                } else {
                    below
                };
                let rect = clamp(WinRect { x: anchor.min.x, y, w, h }, layout);
                Some(Self {
                    id,
                    payload: json!({ "kind": "select", "entries": entries }),
                    kind: PopupKind::Select(select),
                    rect,
                })
            },
            EmbedderControl::ContextMenu(menu) => {
                let mut actions = Vec::new();
                let mut entries = Vec::new();
                let mut height = PAD;
                for item in menu.items() {
                    match item {
                        ContextMenuItem::Item {
                            label,
                            action,
                            enabled,
                        } => {
                            entries.push(json!({
                                "label": label, "value": actions.len(), "enabled": enabled,
                            }));
                            actions.push(*action);
                            height += ROW;
                        },
                        ContextMenuItem::Separator => {
                            entries.push(json!({ "type": "sep" }));
                            height += SEP;
                        },
                    }
                }
                if actions.is_empty() {
                    return None;
                }
                let at = menu.position();
                let w = (240.0 * s) as i32;
                let h = (height * s) as i32;
                let mut rect = WinRect { x: at.min.x, y: at.min.y + layout.ui_h, w, h };
                if rect.y + h > layout.win_h {
                    rect.y -= h;
                }
                if rect.x + w > layout.win_w {
                    rect.x -= w;
                }
                Some(Self {
                    id,
                    payload: json!({ "kind": "menu", "entries": entries }),
                    kind: PopupKind::Menu(menu, actions),
                    rect: clamp(rect, layout),
                })
            },
            EmbedderControl::SimpleDialog(dialog) => {
                let (kind, value, height) = match &dialog {
                    SimpleDialog::Alert(_) => ("alert", String::new(), 170.0),
                    SimpleDialog::Confirm(_) => ("confirm", String::new(), 170.0),
                    SimpleDialog::Prompt(p) => ("prompt", p.current_value().to_owned(), 214.0),
                };
                let w = ((440.0 * s) as i32).min(layout.win_w - (32.0 * s) as i32);
                let rect = WinRect {
                    x: (layout.win_w - w) / 2,
                    y: layout.ui_h + (16.0 * s) as i32,
                    w,
                    h: (height * s) as i32,
                };
                Some(Self {
                    id,
                    payload: json!({
                        "kind": kind, "message": dialog.message(), "value": value,
                        "origin": layout.origin,
                    }),
                    kind: PopupKind::Dialog(dialog),
                    rect: clamp(rect, layout),
                })
            },
            // Seletores de cor/arquivo e IME ainda não têm interface; ao serem descartados,
            // o motor recebe "cancelado".
            _ => None,
        }
    }

    /// Aplica a resposta vinda de `popup.html`.
    pub fn resolve(self, ok: bool, value: Option<Value>) {
        match self.kind {
            PopupKind::Select(mut select) => {
                if let (true, Some(id)) = (ok, value.as_ref().and_then(Value::as_u64)) {
                    select.select(vec![id as usize]);
                }
                select.submit();
            },
            PopupKind::Menu(menu, actions) => {
                match value
                    .as_ref()
                    .and_then(Value::as_u64)
                    .and_then(|i| actions.get(i as usize))
                {
                    Some(action) if ok => menu.select(*action),
                    _ => menu.dismiss(),
                }
            },
            PopupKind::Dialog(SimpleDialog::Prompt(mut prompt)) if ok => {
                if let Some(text) = value.as_ref().and_then(Value::as_str) {
                    prompt.set_current_value(text);
                }
                prompt.confirm();
            },
            PopupKind::Dialog(dialog) => {
                if ok {
                    dialog.confirm();
                } else {
                    dialog.dismiss();
                }
            },
        }
    }

    /// Fecha sem escolha (clique fora, Esc, troca de aba).
    pub fn cancel(self) {
        match self.kind {
            PopupKind::Select(select) => select.submit(),
            PopupKind::Menu(menu, _) => menu.dismiss(),
            PopupKind::Dialog(dialog) => dialog.dismiss(),
        }
    }
}

fn clamp(mut r: WinRect, l: &Layout) -> WinRect {
    r.w = r.w.min(l.win_w).max(1);
    r.h = r.h.min(l.win_h - l.ui_h).max(1);
    r.x = r.x.clamp(0, (l.win_w - r.w).max(0));
    r.y = r.y.clamp(l.ui_h, (l.win_h - r.h).max(l.ui_h));
    r
}
