//! 커넥터: 외부 출처(메신저·메일·이슈 트래커 등)를 **MCP 서버**로 붙인다.
//!
//! 출처마다 코드를 짜지 않는다. 커넥터 하나는 "MCP 서버 하나 + 가져오기 도구 + 답장 도구 + 필드 대응"이다.
//! 서버가 MCP 클라이언트가 되어 주기적으로 가져오기 도구를 부르고, 결과를 수집 항목(`intake.add`)으로 넣는다.
//! 카드가 끝난 뒤 사람이 승인한 답장은 답장 도구로 원래 자리(메시지 스레드 등)에 보낸다.
//!
//! 설정은 워크스페이스의 `.jadecode/connectors.toml`(권한 0600)에 둔다. 토큰 같은 비밀이 들어가므로
//! 저장소에 올리지 않는 `.jadecode/` 아래에 둔다.
//!
//! ```toml
//! [[connector]]
//! name = "slack"
//! project = "webapp"            # 수집 항목이 들어갈 프로젝트
//! command = "npx"
//! args = ["-y", "@modelcontextprotocol/server-slack"]
//! env = { SLACK_BOT_TOKEN = "xoxb-…", SLACK_TEAM_ID = "T…" }
//! interval_sec = 60
//! [connector.fetch]
//! tool = "slack_get_channel_history"
//! args = { channel_id = "C…", limit = 30 }
//! items = "messages"            # 결과 JSON에서 목록이 있는 경로(점으로 구분)
//! id = "ts"                     # 항목의 고유 키
//! title = "text"
//! body = "text"
//! from = "user"
//! url = "https://slack.com/archives/C…/p{id_nodot}"   # 템플릿 가능
//! skip_if = "bot_id"            # 이 키가 있는 항목은 건너뛴다(봇 메시지 등)
//! [connector.reply]
//! tool = "slack_reply_to_thread"
//! args = { channel_id = "C…", thread_ts = "{id}", text = "{text}" }
//! ```
//!
//! 프리셋(`preset = "slack"`)을 쓰면 위 내용을 몇 칸(토큰·팀·채널·프로젝트)으로 만들 수 있다.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct FetchSpec {
    pub tool: String,
    #[serde(default)]
    pub args: Map<String, Value>,
    #[serde(default)]
    pub items: String,
    #[serde(default = "default_id")]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub skip_if: String,
}

fn default_id() -> String {
    "id".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ReplySpec {
    pub tool: String,
    #[serde(default)]
    pub args: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Connector {
    pub name: String,
    pub project: String,
    #[serde(default)]
    pub preset: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_interval")]
    pub interval_sec: u64,
    #[serde(default)]
    pub enabled: Option<bool>,
    pub fetch: FetchSpec,
    #[serde(default)]
    pub reply: Option<ReplySpec>,
}

fn default_interval() -> u64 {
    60
}

impl Connector {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// 화면·RPC로 내보낼 때 비밀(env 값)을 가린다.
    pub fn redacted(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(env) = v.get_mut("env").and_then(|e| e.as_object_mut()) {
            for (_, val) in env.iter_mut() {
                let s = val.as_str().unwrap_or("");
                *val = json!(if s.len() > 8 {
                    format!("{}…", &s[..4])
                } else if s.is_empty() {
                    String::new()
                } else {
                    "…".to_string()
                });
            }
        }
        v
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectorsFile {
    #[serde(default)]
    pub connector: Vec<Connector>,
}

pub fn config_path(root: &Path) -> PathBuf {
    root.join(".jadecode/connectors.toml")
}

pub fn load(root: &Path) -> Result<Vec<Connector>> {
    let p = config_path(root);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).with_context(|| p.display().to_string()),
    };
    let f: ConnectorsFile = toml::from_str(&text).with_context(|| p.display().to_string())?;
    Ok(f.connector)
}

pub fn save(root: &Path, list: &[Connector]) -> Result<()> {
    let p = config_path(root);
    std::fs::create_dir_all(p.parent().unwrap())?;
    let text = toml::to_string(&ConnectorsFile {
        connector: list.to_vec(),
    })?;
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(
        &tmp,
        format!("# jadecode 커넥터 (비밀 포함, 저장소에 올리지 말 것)\n{text}"),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// 프리셋 목록(화면이 입력 칸을 그릴 때 쓴다).
pub fn presets() -> Value {
    json!([
        {
            "preset": "slack",
            "label": "Slack",
            "description": "채널 메시지를 수집하고, 끝난 일의 답장을 그 메시지 스레드에 보낸다.",
            "fields": [
                {"key": "token", "label": "봇 토큰 (xoxb-…)", "secret": true},
                {"key": "team_id", "label": "워크스페이스 ID (T…)"},
                {"key": "channel_id", "label": "채널 ID (C…)"}
            ],
            "requires": "Node.js(npx). 봇 권한: channels:history, chat:write, users:read"
        },
        {
            "preset": "custom",
            "label": "MCP 서버 직접 설정",
            "description": "아무 MCP 서버나 붙인다. 가져오기 도구와 필드 대응을 connectors.toml에 적는다.",
            "fields": [
                {"key": "command", "label": "실행 명령 (예: npx)"},
                {"key": "args", "label": "인자 (공백으로 구분)"},
                {"key": "fetch_tool", "label": "가져오기 도구 이름"}
            ]
        }
    ])
}

/// 프리셋 + 입력값 → 커넥터.
pub fn from_preset(
    preset: &str,
    name: &str,
    project: &str,
    f: &Map<String, Value>,
) -> Result<Connector> {
    let get = |k: &str| {
        f.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    match preset {
        "slack" => {
            let (token, team, channel) = (get("token"), get("team_id"), get("channel_id"));
            if token.is_empty() || channel.is_empty() {
                bail!("Slack 커넥터에는 봇 토큰과 채널 ID가 필요하다");
            }
            let mut env = BTreeMap::new();
            env.insert("SLACK_BOT_TOKEN".into(), token);
            env.insert("SLACK_TEAM_ID".into(), team);
            Ok(Connector {
                name: name.into(),
                project: project.into(),
                preset: "slack".into(),
                command: "npx".into(),
                args: vec!["-y".into(), "@modelcontextprotocol/server-slack".into()],
                env,
                interval_sec: 60,
                enabled: Some(true),
                fetch: FetchSpec {
                    tool: "slack_get_channel_history".into(),
                    args: json!({"channel_id": channel, "limit": 30})
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                    items: "messages".into(),
                    id: "ts".into(),
                    title: "text".into(),
                    body: "text".into(),
                    from: "user".into(),
                    url: format!("https://slack.com/archives/{channel}/p{{id_nodot}}"),
                    skip_if: "bot_id".into(),
                },
                reply: Some(ReplySpec {
                    tool: "slack_reply_to_thread".into(),
                    args: json!({"channel_id": channel, "thread_ts": "{id}", "text": "{text}"})
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                }),
            })
        }
        "custom" => {
            let command = get("command");
            let tool = get("fetch_tool");
            if command.is_empty() || tool.is_empty() {
                bail!("실행 명령과 가져오기 도구 이름이 필요하다");
            }
            Ok(Connector {
                name: name.into(),
                project: project.into(),
                preset: "custom".into(),
                command,
                args: get("args").split_whitespace().map(String::from).collect(),
                env: BTreeMap::new(),
                interval_sec: 120,
                enabled: Some(true),
                fetch: FetchSpec {
                    tool,
                    id: "id".into(),
                    title: "title".into(),
                    body: "body".into(),
                    ..Default::default()
                },
                reply: None,
            })
        }
        other => bail!("모르는 프리셋 {other}"),
    }
}

// ---------- 동기화 상태(메모리) ----------

fn status_map() -> &'static std::sync::Mutex<BTreeMap<String, Value>> {
    static M: std::sync::OnceLock<std::sync::Mutex<BTreeMap<String, Value>>> =
        std::sync::OnceLock::new();
    M.get_or_init(Default::default)
}

/// 마지막 동기화 결과를 남긴다.
pub fn record(name: &str, r: &std::result::Result<usize, crate::rpc::Error>) {
    let v = match r {
        Ok(n) => json!({"ok": true, "added": n, "at": jc_core::event::now_ms()}),
        Err(e) => json!({"ok": false, "error": e.message, "at": jc_core::event::now_ms()}),
    };
    if let Ok(mut m) = status_map().lock() {
        m.insert(name.to_string(), v);
    }
}

pub fn status_of(name: &str) -> Value {
    status_map()
        .lock()
        .ok()
        .and_then(|m| m.get(name).cloned())
        .unwrap_or(Value::Null)
}

// ---------- 결과 → 항목 ----------

/// 점으로 구분한 경로로 JSON을 따라간다. 빈 경로는 자기 자신.
pub fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(v);
    }
    let mut cur = v;
    for seg in path.split('.') {
        cur = match cur {
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            Value::Object(o) => o.get(seg)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn as_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// `{key}` 자리를 채운다. `vars`에 없는 키는 그대로 둔다.
pub fn fill(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

fn fill_value(v: &Value, vars: &BTreeMap<String, String>) -> Value {
    match v {
        Value::String(s) => json!(fill(s, vars)),
        Value::Array(a) => Value::Array(a.iter().map(|x| fill_value(x, vars)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), fill_value(x, vars)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// MCP 도구 결과에서 본문 JSON을 꺼낸다(`structuredContent` 우선, 없으면 첫 텍스트를 JSON으로).
pub fn result_json(result: &Value) -> Value {
    if let Some(s) = result.get("structuredContent") {
        if !s.is_null() {
            return s.clone();
        }
    }
    if let Some(arr) = result["content"].as_array() {
        for c in arr {
            if let Some(t) = c["text"].as_str() {
                if let Ok(v) = serde_json::from_str::<Value>(t) {
                    return v;
                }
                return json!(t);
            }
        }
    }
    result.clone()
}

/// 수집 항목 하나(서버의 `intake.add` 인자 모양).
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    pub raw_id: String,
    pub title: String,
    pub body: String,
    pub from: String,
    pub url: String,
}

pub fn map_items(c: &Connector, data: &Value) -> Vec<Fetched> {
    let list = match at(data, &c.fetch.items) {
        Some(Value::Array(a)) => a.clone(),
        Some(v @ Value::Object(_)) => vec![v.clone()],
        _ => vec![],
    };
    let mut out = vec![];
    for it in &list {
        if !c.fetch.skip_if.is_empty()
            && at(it, &c.fetch.skip_if)
                .map(|v| !v.is_null())
                .unwrap_or(false)
        {
            continue;
        }
        let raw_id = as_text(at(it, &c.fetch.id));
        if raw_id.is_empty() {
            continue;
        }
        let body = as_text(at(it, &c.fetch.body));
        let mut title = as_text(at(it, &c.fetch.title));
        if title.is_empty() {
            title = body.clone();
        }
        let title: String = {
            let one = title.lines().next().unwrap_or("").trim().to_string();
            if one.chars().count() > 80 {
                one.chars().take(80).collect::<String>() + "…"
            } else {
                one
            }
        };
        if title.is_empty() {
            continue;
        }
        let mut vars = BTreeMap::new();
        vars.insert("id".to_string(), raw_id.clone());
        vars.insert("id_nodot".to_string(), raw_id.replace('.', ""));
        let url = if c.fetch.url.contains('{') {
            fill(&c.fetch.url, &vars)
        } else {
            as_text(at(it, &c.fetch.url))
        };
        out.push(Fetched {
            raw_id,
            title,
            body,
            from: as_text(at(it, &c.fetch.from)),
            url,
        });
    }
    out
}

/// 답장 도구 인자.
pub fn reply_args(spec: &ReplySpec, raw_id: &str, text: &str) -> Value {
    let mut vars = BTreeMap::new();
    vars.insert("id".to_string(), raw_id.to_string());
    vars.insert("text".to_string(), text.to_string());
    fill_value(&Value::Object(spec.args.clone()), &vars)
}

// ---------- MCP 클라이언트(stdio) ----------

/// MCP 서버 하나와의 세션. 요청마다 프로세스를 띄우고 닫는다(가끔 부르므로 단순한 쪽을 택했다).
pub struct McpSession {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    next: u64,
}

impl McpSession {
    pub async fn start(c: &Connector) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(&c.command);
        cmd.args(&c.args)
            .envs(&c.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().with_context(|| {
            format!(
                "MCP 서버를 띄우지 못했다: {} {}",
                c.command,
                c.args.join(" ")
            )
        })?;
        let stdin = child.stdin.take().context("stdin")?;
        let stdout = child.stdout.take().context("stdout")?;
        let mut s = Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next: 1,
        };
        s.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "jadecode", "version": env!("CARGO_PKG_VERSION")}}),
        )
        .await
        .context("MCP initialize")?;
        s.notify("notifications/initialized", json!({})).await?;
        Ok(s)
    }

    async fn send(&mut self, v: Value) -> Result<()> {
        let mut line = v.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        let wait = async {
            loop {
                let line = self
                    .lines
                    .next_line()
                    .await?
                    .ok_or_else(|| anyhow!("MCP 서버가 응답 없이 끝났다"))?;
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if v.get("id").and_then(|x| x.as_u64()) != Some(id) {
                    continue;
                }
                if let Some(e) = v.get("error") {
                    bail!("MCP 오류: {}", e["message"].as_str().unwrap_or("?"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        };
        // npx가 처음 패키지를 받는 시간을 감안해 넉넉히 둔다.
        tokio::time::timeout(Duration::from_secs(90), wait)
            .await
            .map_err(|_| anyhow!("MCP 응답 시간 초과({method})"))?
    }

    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        let r = self
            .request("tools/call", json!({"name": name, "arguments": args}))
            .await?;
        if r["isError"].as_bool() == Some(true) {
            bail!("{name} 실패: {}", as_text(r["content"][0].get("text")));
        }
        Ok(r)
    }

    pub async fn tools(&mut self) -> Result<Vec<String>> {
        let r = self.request("tools/list", json!({})).await?;
        Ok(r["tools"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn close(mut self) {
        let _ = self.child.kill().await;
    }
}

/// 가져오기 한 번.
pub async fn fetch(c: &Connector) -> Result<Vec<Fetched>> {
    let mut s = McpSession::start(c).await?;
    let r = s
        .call_tool(&c.fetch.tool, Value::Object(c.fetch.args.clone()))
        .await;
    s.close().await;
    Ok(map_items(c, &result_json(&r?)))
}

/// 답장 한 번.
pub async fn reply(c: &Connector, raw_id: &str, text: &str) -> Result<()> {
    let spec = c
        .reply
        .as_ref()
        .ok_or_else(|| anyhow!("{} 커넥터에는 답장 도구가 없다", c.name))?;
    let mut s = McpSession::start(c).await?;
    let r = s
        .call_tool(&spec.tool, reply_args(spec, raw_id, text))
        .await;
    s.close().await;
    r.map(|_| ())
}

/// 연결 시험: 서버가 뜨고 가져오기 도구가 있는지.
pub async fn test(c: &Connector) -> Result<Vec<String>> {
    let mut s = McpSession::start(c).await?;
    let tools = s.tools().await;
    s.close().await;
    let tools = tools?;
    if !tools.iter().any(|t| t == &c.fetch.tool) {
        bail!(
            "가져오기 도구 {}가 없다. 이 서버의 도구: {}",
            c.fetch.tool,
            tools.join(", ")
        );
    }
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slack() -> Connector {
        let f = json!({"token":"xoxb-123456789","team_id":"T1","channel_id":"C9"});
        from_preset("slack", "slack", "web", f.as_object().unwrap()).unwrap()
    }

    #[test]
    fn slack_preset_maps_messages() {
        let c = slack();
        let data = json!({"ok": true, "messages": [
            {"ts": "1700000000.000100", "user": "U1", "text": "결제 페이지가 500을 냅니다\n자세한 내용…"},
            {"ts": "1700000001.000200", "user": "U2", "text": "봇 알림", "bot_id": "B1"},
            {"ts": "1700000002.000300", "user": "U3", "text": ""}
        ]});
        let items = map_items(&c, &data);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].raw_id, "1700000000.000100");
        assert_eq!(items[0].title, "결제 페이지가 500을 냅니다");
        assert_eq!(items[0].from, "U1");
        assert_eq!(
            items[0].url,
            "https://slack.com/archives/C9/p1700000000000100"
        );
        let args = reply_args(c.reply.as_ref().unwrap(), "1700000000.000100", "고쳤습니다");
        assert_eq!(
            args,
            json!({"channel_id":"C9","thread_ts":"1700000000.000100","text":"고쳤습니다"})
        );
    }

    #[test]
    fn result_json_reads_text_content() {
        let r = json!({"content":[{"type":"text","text":"{\"messages\":[]}"}]});
        assert_eq!(result_json(&r), json!({"messages":[]}));
        let r = json!({"structuredContent":{"a":1},"content":[]});
        assert_eq!(result_json(&r), json!({"a":1}));
    }

    #[test]
    fn redacts_secrets_and_roundtrips() {
        let c = slack();
        let red = c.redacted();
        assert_eq!(red["env"]["SLACK_BOT_TOKEN"], "xoxb…");
        let dir = std::env::temp_dir().join(format!("jc-conn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save(&dir, std::slice::from_ref(&c)).unwrap();
        assert_eq!(load(&dir).unwrap(), vec![c]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(config_path(&dir))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(m & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_lookup() {
        let v = json!({"a":{"b":[{"c":1}]}});
        assert_eq!(at(&v, "a.b.0.c"), Some(&json!(1)));
        assert_eq!(at(&v, "a.x"), None);
    }

    /// 실제 MCP 서버처럼 도는 작은 셸 스크립트로 가져오기·답장을 확인한다.
    #[tokio::test]
    async fn talks_to_stdio_mcp_server() {
        let dir = std::env::temp_dir().join(format!("jc-mcpfake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake.py");
        std::fs::write(
            &script,
            r#"import sys, json
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    meth = m["method"]
    if meth == "initialize":
        r = {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "fake"}}
    elif meth == "tools/list":
        r = {"tools": [{"name": "list_items"}, {"name": "reply"}]}
    elif meth == "tools/call" and m["params"]["name"] == "list_items":
        r = {"content": [{"type": "text", "text": json.dumps({"items": [{"id": "7", "title": "hello", "body": "b"}]})}]}
    else:
        r = {"content": [{"type": "text", "text": "ok " + m["params"]["arguments"]["text"]}]}
    print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}), flush=True)
"#,
        )
        .unwrap();
        if std::process::Command::new("python3")
            .arg("-V")
            .output()
            .is_err()
        {
            return;
        }
        let c = Connector {
            name: "fake".into(),
            project: "p".into(),
            command: "python3".into(),
            args: vec![script.display().to_string()],
            fetch: FetchSpec {
                tool: "list_items".into(),
                items: "items".into(),
                id: "id".into(),
                title: "title".into(),
                body: "body".into(),
                ..Default::default()
            },
            reply: Some(ReplySpec {
                tool: "reply".into(),
                args: json!({"text": "{text}"}).as_object().cloned().unwrap(),
            }),
            ..Default::default()
        };
        assert_eq!(test(&c).await.unwrap(), vec!["list_items", "reply"]);
        let items = fetch(&c).await.unwrap();
        assert_eq!(items[0].raw_id, "7");
        assert_eq!(items[0].title, "hello");
        reply(&c, "7", "done").await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
