//! 봇 정의와 실행 플래그. 봇 = 폴더(`bots/<name>/`), 버전 = 커밋(첫 버전은 폴더 스냅숏).
//!
//! 사람만 정하는 값(권한 정책·예산·사람 절차)은 `policy/`에 두고 봇 폴더 밖에 둔다(C1).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 봇 정의(`bot.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotSpec {
    pub name: String,
    #[serde(default)]
    pub title: String,
    /// 에이전트 종류. 첫 버전은 `claude` 하나.
    #[serde(default = "default_agent")]
    pub agent: String,
    #[serde(default)]
    pub model: Option<String>,
    /// 역할 지침 파일(봇 폴더 기준).
    #[serde(default = "default_instructions")]
    pub instructions: String,
    /// 스킬 폴더(봇 폴더 기준, 있으면 `--plugin-dir`로 넣는다).
    #[serde(default)]
    pub skills: Option<String>,
    /// 기억 폴더(지침 뒤에 덧붙인다).
    #[serde(default)]
    pub memory: Option<String>,
    /// 권한 정책 이름(`policy/permissions/<name>.json`).
    #[serde(default = "default_policy")]
    pub policy: String,
    /// `--add-dir`로 더 열어 줄 경로(워크트리 등).
    #[serde(default)]
    pub add_dirs: Vec<String>,
    /// 허용할 내장 스킬(나머지는 deny).
    #[serde(default)]
    pub allowed_skills: Vec<String>,
    /// 실행 파일 경로(비우면 백엔드 기본값을 PATH에서 찾는다).
    #[serde(default)]
    pub bin: Option<String>,
}

fn default_agent() -> String {
    "claude".into()
}
fn default_instructions() -> String {
    "instructions.md".into()
}
fn default_policy() -> String {
    "default".into()
}

impl BotSpec {
    /// 재시작 설정을 만들 때 채우는 빈 값.
    pub fn placeholder() -> Self {
        Self {
            name: String::new(),
            title: String::new(),
            agent: "claude".into(),
            model: None,
            instructions: "instructions.md".into(),
            skills: None,
            memory: None,
            policy: "default".into(),
            add_dirs: vec![],
            allowed_skills: vec![],
            bin: None,
        }
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let p = dir.join("bot.toml");
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("bot.toml 없음: {}", p.display()))?;
        let spec: BotSpec =
            toml::from_str(&text).with_context(|| format!("parse {}", p.display()))?;
        // 어떤 에이전트인지는 호출자가 `AgentKind::parse`로 확인한다(넷 다 지원).
        Ok(spec)
    }

    /// 지침 + 기억을 합친 텍스트. 인스턴스 사본으로 쓴다.
    pub fn instructions_text(&self, dir: &Path) -> Result<String> {
        let mut out = String::new();
        let main = dir.join(&self.instructions);
        let text = std::fs::read_to_string(&main)
            .with_context(|| format!("instructions 없음: {}", main.display()))?;
        out.push_str(&text);
        if let Some(m) = &self.memory {
            let mp = dir.join(m);
            if mp.is_dir() {
                let mut names: Vec<PathBuf> = std::fs::read_dir(&mp)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
                    .collect();
                names.sort();
                for p in names {
                    out.push_str("\n\n## 기억: ");
                    out.push_str(&p.file_name().unwrap_or_default().to_string_lossy());
                    out.push_str("\n\n");
                    out.push_str(&std::fs::read_to_string(&p)?);
                }
            }
        }
        Ok(out)
    }

    /// 스킬 폴더(있으면).
    pub fn skills_dir(&self, dir: &Path) -> Option<PathBuf> {
        let s = self.skills.as_ref()?;
        let p = dir.join(s);
        if p.is_dir() {
            Some(p)
        } else {
            None
        }
    }
}

/// 인스턴스를 띄울 때 쓰는 격리 선택.
#[derive(Debug, Clone, Default)]
pub struct Isolation {
    /// 사용자 설정을 모두 뺀다(`--setting-sources ""`). 첫 버전은 항상 참.
    pub clean_settings: bool,
}

/// `policy/permissions/<name>.json` — Claude Code `--settings` 파일.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PermissionPolicy {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub default_mode: Option<String>,
}

impl PermissionPolicy {
    pub fn load(root: &Path, name: &str) -> Result<Self> {
        let p = root.join("policy/permissions").join(format!("{name}.json"));
        if !p.exists() {
            return Ok(Self::default_policy());
        }
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p)?)?;
        let perms = v.get("permissions").cloned().unwrap_or(v);
        Ok(Self {
            allow: perms
                .get("allow")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            deny: perms
                .get("deny")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            default_mode: perms
                .get("defaultMode")
                .and_then(|x| x.as_str())
                .map(String::from),
        })
    }

    /// 기본 정책: 사람 절차·네트워크 명령은 deny, 읽기·테스트는 allow.
    pub fn default_policy() -> Self {
        Self {
            allow: [
                // jadecode 자신의 MCP 도구는 항상 허용한다(보고·질문·ack). 이걸 빼면
                // 도구 호출마다 권한 RPC가 돌아 턴이 느려지고 사람이 매번 답해야 한다.
                "mcp__jadecode",
                "ToolSearch",
                "Read",
                "Glob",
                "Grep",
                "Edit",
                "Write",
                "TodoWrite",
                "Bash(git status:*)",
                "Bash(git diff:*)",
                "Bash(git log:*)",
                "Bash(git add:*)",
                "Bash(git commit:*)",
                "Bash(ls:*)",
                "Bash(cat:*)",
                "Bash(cargo test:*)",
                "Bash(cargo build:*)",
                "Bash(cargo run:*)",
                "Bash(npm test:*)",
                "Bash(npm run build:*)",
                "Bash(python3:*)",
                "Bash(jq:*)",
                "Bash(mkdir:*)",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            deny: [
                "Bash(git push:*)",
                "Bash(curl:*)",
                "Bash(wget:*)",
                "Bash(rm -rf:*)",
                "Bash(sudo:*)",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            default_mode: Some("acceptEdits".into()),
        }
    }

    /// `--settings`에 쓸 JSON. 허용하지 않은 내장 스킬은 deny한다(S1).
    pub fn to_settings_json(&self, allowed_skills: &[String]) -> serde_json::Value {
        let mut allow = self.allow.clone();
        if !allow.iter().any(|a| a == "mcp__jadecode") {
            allow.push("mcp__jadecode".into());
        }
        if !allow.iter().any(|a| a == "ToolSearch") {
            allow.push("ToolSearch".into());
        }
        const BUILTIN_SKILLS: &[&str] = &[
            "update-config",
            "keybindings-help",
            "simplify",
            "fewer-permission-prompts",
            "loop",
            "claude-api",
            "run",
            "init",
            "review",
            "security-review",
            "statusline-setup",
            "insights",
            "output-style-setup",
            "export",
            "pr-comments",
            "todos",
            "agent-sdk-verifier-ts",
            "agent-sdk-verifier-py",
            "code-review",
        ];
        let mut deny = self.deny.clone();
        for s in BUILTIN_SKILLS {
            if !allowed_skills.iter().any(|a| a == s) {
                deny.push(format!("Skill({s})"));
            }
        }
        serde_json::json!({
            "permissions": {"allow": allow, "deny": deny,
                            "defaultMode": self.default_mode.clone().unwrap_or_else(|| "acceptEdits".into())},
            // 사용자 설정이 빠지면 기본 attribution 문구로 돌아간다 → 명시한다(S1).
            "attribution": {"commit": "", "pr": ""},
        })
    }
}

/// MCP 서버 설정(`--mcp-config`) — jadecode MCP 서버 하나.
pub fn mcp_config(
    server_name: &str,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            server_name: {
                "type": "stdio",
                "command": command,
                "args": args,
                "env": env,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_denies_human_procedures() {
        let p = PermissionPolicy::default_policy();
        let j = p.to_settings_json(&[]);
        let deny = j["permissions"]["deny"].as_array().unwrap();
        assert!(deny
            .iter()
            .any(|x| x.as_str().unwrap().contains("git push")));
        assert!(deny
            .iter()
            .any(|x| x.as_str().unwrap().starts_with("Skill(")));
        assert_eq!(j["attribution"]["commit"], "");
    }

    #[test]
    fn allowed_skills_are_not_denied() {
        let p = PermissionPolicy::default_policy();
        let j = p.to_settings_json(&["review".to_string()]);
        let deny = j["permissions"]["deny"].as_array().unwrap();
        assert!(!deny.iter().any(|x| x.as_str().unwrap() == "Skill(review)"));
        assert!(deny.iter().any(|x| x.as_str().unwrap() == "Skill(init)"));
    }
}

/// ACP `session/new`의 `mcpServers` 항목(jadecode MCP 서버 하나).
pub fn isolation_mcp_servers(
    socket: &std::path::Path,
    instance: &str,
    card: &Option<String>,
    project: &str,
) -> serde_json::Value {
    // ACP는 stdio MCP를 `{name, command, args, env:[]}`로 받는다(실측).
    let mut args = vec![
        "--socket".to_string(),
        socket.display().to_string(),
        "--role".to_string(),
        if card.is_none() {
            "orchestrator".to_string()
        } else {
            "worker".to_string()
        },
        "--instance".to_string(),
        instance.to_string(),
        "--project".to_string(),
        project.to_string(),
    ];
    if let Some(c) = card {
        args.push("--card".to_string());
        args.push(c.clone());
    }
    serde_json::json!([{
        "name": "jadecode",
        "command": mcp_command(),
        "args": args,
        "env": [],
    }])
}

/// MCP 서버 실행 파일: 환경변수 → 실행 파일 옆 → PATH.
pub fn mcp_command() -> String {
    if let Some(p) = std::env::var_os("JADECODE_MCP") {
        return p.to_string_lossy().to_string();
    }
    if let Ok(exe) = std::env::current_exe() {
        let sib = exe.with_file_name("jadecode-mcp");
        if sib.exists() {
            return sib.to_string_lossy().to_string();
        }
    }
    "jadecode-mcp".to_string()
}

#[cfg(test)]
mod mcp_tests {
    use super::*;

    #[test]
    fn worker_mcp_entry_has_card() {
        let v = isolation_mcp_servers(
            std::path::Path::new("/tmp/s.sock"),
            "w-1",
            &Some("work/p/c".to_string()),
            "p",
        );
        assert_eq!(v[0]["name"], "jadecode");
        let args: Vec<String> = v[0]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        assert!(args.contains(&"--card".to_string()));
        assert!(args.contains(&"worker".to_string()));
    }

    #[test]
    fn orchestrator_has_no_card() {
        let v = isolation_mcp_servers(std::path::Path::new("/tmp/s.sock"), "o-1", &None, "p");
        let args: Vec<String> = v[0]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        assert!(args.contains(&"orchestrator".to_string()));
        assert!(!args.contains(&"--card".to_string()));
    }
}
