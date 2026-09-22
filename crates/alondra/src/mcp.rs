use std::io::{BufRead, Write};
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

use crate::client::Client;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Node,
    Orchestrator,
}

const INSTRUCTIONS_NODE: &str = "Alondra conecta este nodo con David y con el orquestador. Si necesitas que David decida algo, usa `ask` con un título de una línea y todo el contexto en `detail`, enlaces a PRs y ficheros adjuntos; después termina tu turno: la respuesta te llegará sola como mensaje. Cuando entregues algo (una PR, un informe, un documento) usa `report`. Si estás parado por falta de máquina, usa `wait_for_machine`.";
const INSTRUCTIONS_ORCHESTRATOR: &str = "Alondra es tu sistema nervioso: estado fiable de la flota (`fleet`, `node`), capacidad de la máquina (`machine`), y manos (`launch`, `send`). Las preguntas de los nodos a David le llegan a él directamente; tú recibes en tu bandeja lo que es para ti. Usa `ask` para preguntarle algo a David y `report` para informarle.";

fn ask_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string", "description": "La pregunta en una línea: es lo que David ve en la notificación"},
            "detail": {"type": "string", "description": "Todo el contexto que David necesita para contestar sin preguntarte nada (markdown)"},
            "options": {"type": "array", "items": {"type": "object", "properties": {"label": {"type": "string"}, "description": {"type": "string"}}, "required": ["label"]}, "description": "Opciones posibles; David siempre puede contestar con texto libre"},
            "links": {"type": "array", "items": {"type": "object", "properties": {"url": {"type": "string"}, "title": {"type": "string"}}, "required": ["url"]}, "description": "PRs, commits o URLs relevantes"},
            "attachments": {"type": "array", "items": {"type": "string"}, "description": "Rutas absolutas de ficheros que David debe ver"},
            "blocking": {"type": "boolean", "description": "true si no puedes seguir sin la respuesta (por defecto true)"},
            "to_orchestrator": {"type": "boolean", "description": "true si la pregunta es para el orquestador y no para David"}
        },
        "required": ["title"]
    })
}

fn report_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string", "description": "Qué entregas, en una línea"},
            "body": {"type": "string", "description": "El informe (markdown), breve: lo medido, lo que cambia y lo que queda"},
            "kind": {"type": "string", "enum": ["delivery", "finding", "progress", "final"], "description": "final = has terminado y no queda nada pendiente"},
            "links": {"type": "array", "items": {"type": "object", "properties": {"url": {"type": "string"}, "title": {"type": "string"}}, "required": ["url"]}},
            "attachments": {"type": "array", "items": {"type": "string"}, "description": "Rutas absolutas de documentos que entregas"}
        },
        "required": ["title"]
    })
}

fn tools(role: Role) -> Value {
    let mut list = vec![
        json!({"name": "ask", "description": "Pregunta estructurada a David (o al orquestador). Vuelve al momento con el número; la respuesta llega como mensaje cuando termines el turno.", "inputSchema": ask_schema()}),
        json!({"name": "report", "description": "Informa a David de una entrega, un hallazgo o de que has terminado.", "inputSchema": report_schema()}),
        json!({"name": "wait_for_machine", "description": "Declara que estás parado esperando recursos de la máquina (RAM, compilación, límite de uso).", "inputSchema": {"type": "object", "properties": {"reason": {"type": "string"}}, "required": ["reason"]}}),
    ];
    if role == Role::Orchestrator {
        list.extend([
            json!({"name": "fleet", "description": "Estado de todos los nodos, preguntas abiertas y máquina.", "inputSchema": {"type": "object", "properties": {}}}),
            json!({"name": "node", "description": "Detalle de un nodo: estado, herramienta en curso, último mensaje, preguntas, informes y línea de tiempo.", "inputSchema": {"type": "object", "properties": {"node": {"type": "string", "description": "Nombre, id corto o session_id"}}, "required": ["node"]}}),
            json!({"name": "machine", "description": "Capacidad de la máquina: memoria, carga, CPU y disco.", "inputSchema": {"type": "object", "properties": {}}}),
            json!({"name": "send", "description": "Manda un mensaje a un nodo; le llega en cuanto termine su turno (o al momento si está parado).", "inputSchema": {"type": "object", "properties": {"node": {"type": "string"}, "text": {"type": "string"}}, "required": ["node", "text"]}}),
            json!({"name": "launch", "description": "Lanza un nodo nuevo con el plugin de Alondra.", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "cwd": {"type": "string"}, "prompt": {"type": "string"}, "model": {"type": "string"}}, "required": ["name", "cwd", "prompt"]}}),
        ]);
    }
    Value::Array(list)
}

pub fn caller() -> Value {
    let session = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    json!({"session_id": session, "pids": ancestors()})
}

fn ancestors() -> Vec<i32> {
    let mut out = vec![];
    let mut pid = unsafe { libc::getppid() };
    for _ in 0..4 {
        if pid <= 1 {
            break;
        }
        out.push(pid);
        let parent = std::process::Command::new("/bin/ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok());
        match parent {
            Some(p) => pid = p,
            None => break,
        }
    }
    out
}

fn merge(mut a: Value, b: &Value) -> Value {
    if let (Some(a), Some(b)) = (a.as_object_mut(), b.as_object()) {
        for (k, v) in b {
            a.insert(k.clone(), v.clone());
        }
    }
    a
}

fn call(client: &Client, role: Role, name: &str, args: &Value) -> Result<String> {
    let who = caller();
    Ok(match name {
        "ask" => {
            let r = client.post("/v1/questions", &merge(args.clone(), &who))?;
            let id = r.get("id").and_then(Value::as_u64).unwrap_or(0);
            format!("Pregunta #{id} registrada y enviada a David. Termina tu turno cuando no tengas nada más que hacer: la respuesta te llegará sola como mensaje.")
        }
        "report" => {
            let r = client.post("/v1/reports", &merge(args.clone(), &who))?;
            format!("Informe #{} entregado a David.", r.get("id").and_then(Value::as_u64).unwrap_or(0))
        }
        "wait_for_machine" => {
            let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
            let body = merge(json!({"title": format!("Esperando máquina: {reason}"), "kind": "progress"}), &who);
            client.post("/v1/reports", &body)?;
            "Anotado: estás esperando máquina.".to_string()
        }
        "fleet" if role == Role::Orchestrator => serde_json::to_string_pretty(&client.get::<Value>("/v1/overview")?)?,
        "machine" if role == Role::Orchestrator => {
            let v: Value = client.get("/v1/overview")?;
            serde_json::to_string_pretty(v.get("machine").unwrap_or(&Value::Null))?
        }
        "node" if role == Role::Orchestrator => {
            let key = args.get("node").and_then(Value::as_str).unwrap_or("");
            serde_json::to_string_pretty(&client.get::<Value>(&format!("/v1/nodes/{}", urlencode(key)))?)?
        }
        "send" if role == Role::Orchestrator => {
            let key = args.get("node").and_then(Value::as_str).unwrap_or("");
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            client.post(&format!("/v1/nodes/{}/messages", urlencode(key)), &json!({"text": text, "from": "orchestrator"}))?;
            format!("Mensaje en la bandeja de {key}.")
        }
        "launch" if role == Role::Orchestrator => serde_json::to_string_pretty(&client.post("/v1/launch", args)?)?,
        other => anyhow::bail!("herramienta desconocida: {other}"),
    })
}

pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn serve(role: Role) -> Result<()> {
    let client = Client::new(Duration::from_secs(30));
    if role == Role::Orchestrator {
        let _ = client.post("/v1/orchestrator", &caller());
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = req.get("id").cloned() else { continue };
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "alondra", "version": env!("CARGO_PKG_VERSION")},
                "instructions": if role == Role::Orchestrator { INSTRUCTIONS_ORCHESTRATOR } else { INSTRUCTIONS_NODE },
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools(role)})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match call(&client, role, name, &args) {
                    Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
                    Err(e) => json!({"content": [{"type": "text", "text": format!("Error de Alondra: {e}")}], "isError": true}),
                })
            }
            _ => Err(json!({"code": -32601, "message": format!("método no soportado: {method}")})),
        };
        let resp = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        writeln!(stdout, "{resp}")?;
        stdout.flush()?;
    }
    Ok(())
}
