//! 화면에 그릴 만큼만 마크다운을 블록으로 나눈다(제목·문단·목록·체크·코드·인용·구분선).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdBlock {
    Heading(u8, String),
    Para(String),
    Bullet(u8, String),
    Check(bool, String),
    Code(Vec<String>),
    Quote(String),
    Rule,
}

/// 앞머리(`---` … `---`)를 떼고 블록으로 나눈다.
pub fn markdown_blocks(md: &str) -> Vec<MdBlock> {
    let mut lines = md.lines().peekable();
    // 앞머리
    if md.starts_with("---") {
        lines.next();
        for l in lines.by_ref() {
            if l.trim() == "---" {
                break;
            }
        }
    }
    let mut out: Vec<MdBlock> = vec![];
    let mut para: Vec<String> = vec![];
    let flush = |para: &mut Vec<String>, out: &mut Vec<MdBlock>| {
        if !para.is_empty() {
            out.push(MdBlock::Para(para.join(" ")));
            para.clear();
        }
    };
    while let Some(l) = lines.next() {
        let t = l.trim_end();
        let trimmed = t.trim_start();
        if trimmed.starts_with("```") {
            flush(&mut para, &mut out);
            let mut code = vec![];
            for c in lines.by_ref() {
                if c.trim_start().starts_with("```") {
                    break;
                }
                code.push(c.to_string());
            }
            out.push(MdBlock::Code(code));
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut para, &mut out);
            continue;
        }
        if let Some(h) = heading(trimmed) {
            flush(&mut para, &mut out);
            out.push(h);
            continue;
        }
        if trimmed == "---" || trimmed == "***" {
            flush(&mut para, &mut out);
            out.push(MdBlock::Rule);
            continue;
        }
        let indent = ((t.len() - trimmed.len()) / 2).min(4) as u8;
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            flush(&mut para, &mut out);
            if let Some(x) = rest.strip_prefix("[ ] ") {
                out.push(MdBlock::Check(false, x.to_string()));
            } else if let Some(x) = rest
                .strip_prefix("[x] ")
                .or_else(|| rest.strip_prefix("[X] "))
            {
                out.push(MdBlock::Check(true, x.to_string()));
            } else {
                out.push(MdBlock::Bullet(indent, rest.to_string()));
            }
            continue;
        }
        if let Some(pos) = trimmed.find(". ") {
            if pos > 0 && pos <= 3 && trimmed[..pos].chars().all(|c| c.is_ascii_digit()) {
                flush(&mut para, &mut out);
                out.push(MdBlock::Bullet(indent, trimmed.to_string()));
                continue;
            }
        }
        if let Some(q) = trimmed.strip_prefix("> ") {
            flush(&mut para, &mut out);
            out.push(MdBlock::Quote(q.to_string()));
            continue;
        }
        para.push(trimmed.to_string());
    }
    flush(&mut para, &mut out);
    out
}

fn heading(l: &str) -> Option<MdBlock> {
    let n = l.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&n) && l[n..].starts_with(' ') {
        Some(MdBlock::Heading(n as u8, l[n + 1..].trim().to_string()))
    } else {
        None
    }
}

/// 인라인 서식(`**굵게**`, `` `코드` ``)을 화면에서 지운다. 글자는 그대로 둔다.
pub fn strip_inline(s: &str) -> String {
    s.replace("**", "").replace("__", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_blocks() {
        let b = markdown_blocks("# 제목\n\n문단 한 줄\n이어짐\n\n- 항목\n  - 안쪽\n- [x] 끝\n- [ ] 남음\n1. 첫째\n> 인용\n---\n```\ncode\n```");
        assert_eq!(b[0], MdBlock::Heading(1, "제목".into()));
        assert_eq!(b[1], MdBlock::Para("문단 한 줄 이어짐".into()));
        assert_eq!(b[2], MdBlock::Bullet(0, "항목".into()));
        assert_eq!(b[3], MdBlock::Bullet(1, "안쪽".into()));
        assert_eq!(b[4], MdBlock::Check(true, "끝".into()));
        assert_eq!(b[5], MdBlock::Check(false, "남음".into()));
        assert_eq!(b[6], MdBlock::Bullet(0, "1. 첫째".into()));
        assert_eq!(b[7], MdBlock::Quote("인용".into()));
        assert_eq!(b[8], MdBlock::Rule);
        assert_eq!(b[9], MdBlock::Code(vec!["code".into()]));
    }
}
