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

fn instructions(role: Role, user: &str) -> String {
    let who = format!("La persona que supervisa la flota se llama {user}: donde las herramientas y el skill de Alondra dicen «el usuario», es {user}.");
    match role {
        Role::Node => format!("Alondra conecta este nodo con {user} y con el orquestador. Si necesitas que {user} decida algo, usa `ask` con un título de una línea y todo el contexto en `detail`, enlaces a PRs y ficheros adjuntos; después termina tu turno: la respuesta te llegará sola como mensaje. Cuando entregues algo (una PR, un informe, un documento) usa `report`. Si estás parado por falta de máquina, usa `wait_for_machine`. {who}"),
        Role::Orchestrator => format!("Alondra es tu sistema nervioso: estado fiable de la flota (`fleet`, `node`), capacidad de la máquina (`machine`), y manos (`launch`, `send`). Las preguntas de los nodos a {user} le llegan directamente; tú recibes en tu bandeja lo que es para ti. Usa `ask` para preguntarle algo a {user} y `report` para informarle. {who}"),
    }
}

fn ask_schema(user: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string", "description": format!("La pregunta en una línea: es lo que {user} ve en la notificación")},
            "detail": {"type": "string", "description": format!("Todo el contexto que {user} necesita para contestar sin preguntarte nada (markdown)")},
            "options": {"type": "array", "items": {"type": "object", "properties": {"label": {"type": "string"}, "description": {"type": "string"}}, "required": ["label"]}, "description": format!("Opciones posibles; {user} siempre puede contestar con texto libre")},
            "links": {"type": "array", "items": {"type": "object", "properties": {"url": {"type": "string"}, "title": {"type": "string"}}, "required": ["url"]}, "description": "PRs, commits o URLs relevantes"},
            "attachments": {"type": "array", "items": {"type": "string"}, "description": format!("Rutas absolutas de ficheros que {user} debe ver (p. ej. el plan que debe aprobar)")},
            "blocking": {"type": "boolean", "description": "true si no puedes seguir sin la respuesta (por defecto true)"},
            "to_orchestrator": {"type": "boolean", "description": format!("true si la pregunta es para el orquestador y no para {user}")}
        },
        "required": ["title"]
    })
}

fn report_schema(user: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string", "description": "Qué entregas, en una línea"},
            "body": {"type": "string", "description": "El informe (markdown), breve: lo medido, lo que cambia y lo que queda"},
            "kind": {"type": "string", "enum": ["notice", "document", "final"], "description": format!("notice = algo que {user} tiene que saber (breve). document = un documento para que lo lea (plan, informe largo): adjunta la ruta en attachments. final = has terminado y no queda nada pendiente. Si necesitas que {user} APRUEBE algo, no uses report: usa ask con el documento adjunto.")},
            "links": {"type": "array", "items": {"type": "object", "properties": {"url": {"type": "string"}, "title": {"type": "string"}}, "required": ["url"]}},
            "attachments": {"type": "array", "items": {"type": "string"}, "description": "Rutas absolutas de documentos que entregas"}
        },
        "required": ["title"]
    })
}

fn tools(role: Role, user: &str) -> Value {
    let mut list = vec![
        json!({"name": "ask", "description": format!("Pregunta estructurada a {user} (o al orquestador). Vuelve al momento con el número; la respuesta llega como mensaje cuando termines el turno."), "inputSchema": ask_schema(user)}),
        json!({"name": "report", "description": format!("Algo para que {user} lo lea, sin decisión pendiente: un aviso breve, un documento largo (plan, informe) o que has terminado. Le llega a su bandeja; si comenta algo, te llega como mensaje."), "inputSchema": report_schema(user)}),
        json!({"name": "wait_for_machine", "description": "Declara que estás parado esperando recursos de la máquina (RAM, compilación, límite de uso).", "inputSchema": {"type": "object", "properties": {"reason": {"type": "string"}}, "required": ["reason"]}}),
    ];
    if role == Role::Orchestrator {
        list.extend([
            json!({"name": "fleet", "description": "Resumen de la flota: nodos no terminados con su estado, preguntas abiertas, informes sin leer y máquina. Para el detalle de uno, `node`.", "inputSchema": {"type": "object", "properties": {}}}),
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

fn call(client: &Client, role: Role, user: &str, name: &str, args: &Value) -> Result<String> {
    let who = caller();
    Ok(match name {
        "ask" => {
            let r = client.post("/v1/questions", &merge(args.clone(), &who))?;
            let id = r.get("id").and_then(Value::as_u64).unwrap_or(0);
            format!("Pregunta #{id} registrada y enviada a {user}. Termina tu turno cuando no tengas nada más que hacer: la respuesta te llegará sola como mensaje.")
        }
        "report" => {
            let r = client.post("/v1/reports", &merge(args.clone(), &who))?;
            format!("Informe #{} entregado a {user}.", r.get("id").and_then(Value::as_u64).unwrap_or(0))
        }
        "wait_for_machine" => {
            let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
            client.post("/v1/waiting-machine", &merge(json!({"reason": reason}), &who))?;
            "Anotado: estás esperando máquina.".to_string()
        }
        "fleet" if role == Role::Orchestrator => serde_json::to_string_pretty(&fleet_summary(&client.get::<Value>("/v1/overview")?))?,
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

fn fleet_summary(o: &Value) -> Value {
    let list = |k: &str| o.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    let pick = |v: &Value, keys: &[&str]| {
        Value::Object(keys.iter().filter_map(|k| v.get(*k).filter(|x| !x.is_null()).map(|x| (k.to_string(), x.clone()))).collect())
    };
    let nodes = list("nodes");
    let done = nodes.iter().filter(|n| n.get("state").and_then(Value::as_str) == Some("done")).count();
    let active: Vec<Value> = nodes
        .iter()
        .filter(|n| n.get("state").and_then(Value::as_str) != Some("done"))
        .map(|n| pick(n, &["name", "short_id", "project", "state", "state_since", "daemon_needs", "waiting_permission", "alive"]))
        .collect();
    let questions: Vec<Value> = list("open_questions").iter().map(|q| pick(q, &["id", "title", "node_name", "origin", "to_orchestrator", "asked_at"])).collect();
    let unread: Vec<Value> = list("reports")
        .iter()
        .filter(|r| r.get("read") == Some(&Value::Bool(false)))
        .map(|r| pick(r, &["id", "title", "kind", "node_name"]))
        .collect();
    json!({"nodes": active, "done_nodes": done, "open_questions": questions, "unread_reports": unread,
           "orchestrator_messages": o.get("orchestrator_messages"), "machine": o.get("machine")})
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
    let user = crate::client::config().name.clone();
    let client = Client::new(Duration::from_secs(30));
    let mut registered = role != Role::Orchestrator;
    let mut register = |client: &Client| {
        if !registered {
            registered = client.post("/v1/orchestrator", &caller()).is_ok();
        }
    };
    register(&client);
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
                "instructions": instructions(role, &user),
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools(role, &user)})),
            "tools/call" => {
                register(&client);
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match call(&client, role, &user, name, &args) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configured_name_reaches_every_text_a_node_reads() {
        for role in [Role::Node, Role::Orchestrator] {
            let text = format!("{} {}", instructions(role, "Mostafa"), tools(role, "Mostafa"));
            assert!(text.contains("Mostafa") && !text.contains("David"), "{text}");
        }
    }
}
