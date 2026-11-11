//! 워크스페이스 기본 봇·정책 파일. `jadecode init`이 만든다.
//!
//! 봇 정의(bots/<name>/bot.toml)와 사람이 정하는 값(policy/)을 나눈다(C1).
//! 자기개선(RSI)은 범위에서 빠졌으므로 버전 관리는 git에 맡긴다.

use anyhow::Result;
use std::path::Path;

pub const WORKER_INSTRUCTIONS: &str = r#"# 개발 봇

너는 jadecode의 워커다. 한 번에 카드 하나에서 일한다.

## 일하는 법

- 카드 폴더(`task.md`)와 레포 워크트리를 읽고, 카드의 목표·완료 조건을 만족시킨다.
- 단계가 끝났으면 `report` 도구로 `status=done`, 계속 중이면 `progress`, 막혔으면 `blocked`를 남긴다.
  보고에는 무엇을 했고 무엇이 남았는지, 근거(명령·파일·링크)를 적는다.
- 답이 필요하면 `ask`로 묻는다. 답은 새 메시지로 온다. 묻고 나면 할 수 있는 일을 계속한다.
- 사람 승인이 필요한 일(보호 브랜치 병합, 배포, 외부로 나가는 메시지, 예산 초과, 프로젝트 삭제)은
  직접 하지 않고 `propose`로 올린다.
- 커밋·push·Draft PR은 네가 한다. 단 `main` 병합은 하지 않는다.
- 경로는 cwd 기준으로 쓴다. `cd`로 다른 git 루트에 옮기지 않는다(`git -C <경로>`).
- 커밋 메시지는 `<type>: <설명>`, 작성자 정보는 건드리지 않는다.
"#;

pub const ORCHESTRATOR_INSTRUCTIONS: &str = r#"# 오케스트레이터 봇

너는 jadecode의 오케스트레이터다. 프로젝트 하나를 맡는다.

## 일하는 법

- 사람의 의도를 받아 카드를 만들고 경로(단계)를 정한다. 카드 추가·이동은 `jadecode` 도구로 한다.
- 워커 인스턴스를 띄우고(`instance.start`), 메시지를 주고받는다(`send`·`reply`).
- 깨우기 다이제스트가 오면 그 안의 이벤트를 읽고 필요한 조치를 한다. 처리한 마지막 seq를 `ack`로 남긴다.
- 사람 결정이 필요한 일은 `escalate`로 올린다. 네가 승인하지 않는다.
- 관찰한 것을 프로젝트 기억(`memory/`)에 짧게 남긴다. 길게 쓰지 않는다.
"#;

pub const BOT_TOML_WORKER: &str = r#"name = "worker"
title = "개발 봇"
agent = "claude"
model = "sonnet"
instructions = "instructions.md"
policy = "default"
allowed_skills = []
"#;

pub const BOT_TOML_ORCHESTRATOR: &str = r#"name = "orchestrator"
title = "오케스트레이터 봇"
agent = "claude"
model = "sonnet"
instructions = "instructions.md"
policy = "orchestrator"
allowed_skills = []
"#;

pub const POLICY_TOML: &str = r#"# 사람이 정하는 값. 봇 정의(자기개선 대상)와 분리한다(C1).

# 사람이 답하지 않으면 권한 요청을 거절하는 시간(분). CLI 무응답 중단(1800초)보다 짧아야 한다.
permission_timeout_min = 25

# 봇 수 상한·예산은 아직 강제하지 않는다(첫 버전).
bots_max = 10

# 사람 절차 기본값. 봇은 이 목록의 일을 직접 하지 않고 propose로 올린다.
human_procedures = [
  "merge_protected_branch",
  "deploy",
  "external_message",
  "budget_exceeded",
  "project_delete",
]
"#;

pub const PERMISSIONS_ORCHESTRATOR: &str = r#"{
  "permissions": {
    "allow": [
      "Read", "Glob", "Grep", "Write", "Edit", "TodoWrite",
      "Bash(jadecode:*)", "Bash(git status:*)", "Bash(git log:*)", "Bash(git diff:*)",
      "Bash(ls:*)", "Bash(cat:*)", "Bash(jq:*)"
    ],
    "deny": [
      "Bash(git push:*)", "Bash(curl:*)", "Bash(wget:*)", "Bash(rm -rf:*)", "Bash(sudo:*)"
    ],
    "defaultMode": "acceptEdits"
  }
}
"#;

/// 워크스페이스에 기본 봇 네 개(오케스트레이터·개발·리뷰·조사)와 정책을 심는다.
pub fn install(root: &Path) -> Result<Vec<String>> {
    let mut created = vec![];
    let bots: &[(&str, &str, &str)] = &[
        ("orchestrator", BOT_TOML_ORCHESTRATOR, ORCHESTRATOR_INSTRUCTIONS),
        ("worker", BOT_TOML_WORKER, WORKER_INSTRUCTIONS),
        ("reviewer", BOT_TOML_WORKER, REVIEW_INSTRUCTIONS),
        ("researcher", BOT_TOML_WORKER, RESEARCH_INSTRUCTIONS),
    ];
    for (name, toml, instructions) in bots {
        let dir = root.join("bots").join(name);
        std::fs::create_dir_all(&dir)?;
        let toml = toml.replace("name = \"worker\"", &format!("name = \"{name}\""));
        let toml = toml.replace("title = \"개발 봇\"", &format!("title = \"{}\"", title_of(name)));
        write_if_absent(&dir.join("bot.toml"), &toml)?;
        write_if_absent(&dir.join("instructions.md"), instructions)?;
        std::fs::create_dir_all(dir.join("memory"))?;
        created.push(format!("bots/{name}"));
    }
    let perms = root.join("policy/permissions");
    std::fs::create_dir_all(&perms)?;
    write_if_absent(&root.join("policy/policy.toml"), POLICY_TOML)?;
    write_if_absent(&perms.join("orchestrator.json"), PERMISSIONS_ORCHESTRATOR)?;
    created.push("policy/".into());
    Ok(created)
}

fn title_of(name: &str) -> &'static str {
    match name {
        "orchestrator" => "오케스트레이터 봇",
        "worker" => "개발 봇",
        "reviewer" => "리뷰 봇",
        _ => "조사 봇",
    }
}

fn write_if_absent(p: &Path, content: &str) -> Result<()> {
    if p.exists() {
        return Ok(());
    }
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, content)?;
    Ok(())
}

pub const REVIEW_INSTRUCTIONS: &str = r#"# 리뷰 봇

너는 jadecode의 리뷰 봇이다. 카드의 변경(diff)을 읽고 결함을 찾는다.

- 파일을 쓰지 않는다. 읽고 판정만 한다.
- 지적은 파일·줄·이유·고치는 방법 순서로 짧게 적는다.
- 통과 판정이면 `report(status=done, summary="통과: <근거>")`, 아니면 `blocked`로 남긴다.
"#;

pub const RESEARCH_INSTRUCTIONS: &str = r#"# 조사 봇

너는 jadecode의 조사 봇이다. 질문에 답할 근거를 모아 정리한다.

- 파일을 쓰지 않는다(조사 결과는 `report` 본문에 적는다).
- 출처를 붙인다. 확인하지 못한 것은 "미확인"이라고 적는다.
- 끝나면 `report(status=done)`으로 요약한다.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_four_bots_and_policy() {
        let d = std::env::temp_dir().join(format!("jc-bots-{}-{}", std::process::id(), crate::ulid::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        let made = install(&d).unwrap();
        assert_eq!(made.len(), 5, "{made:?}");
        for n in ["orchestrator", "worker", "reviewer", "researcher"] {
            let p = d.join("bots").join(n).join("bot.toml");
            assert!(p.exists(), "{}", p.display());
            let text = std::fs::read_to_string(&p).unwrap();
            let t: toml::Table = text.parse().unwrap();
            assert_eq!(t.get("name").and_then(|v| v.as_str()), Some(n));
        }
        assert!(d.join("policy/policy.toml").exists());
        assert!(d.join("policy/permissions/orchestrator.json").exists());
        // 두 번 불러도 덮어쓰지 않는다
        std::fs::write(d.join("bots/worker/instructions.md"), "custom").unwrap();
        install(&d).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("bots/worker/instructions.md")).unwrap(), "custom");
    }
}
