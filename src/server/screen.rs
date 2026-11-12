use super::*;

pub(super) fn screen_lines(term: &Term<Listener>) -> Vec<String> {
    use alacritty_terminal::index::{Column, Line};
    let g = term.grid();
    (0..g.screen_lines() as i32)
        .map(|l| {
            let row = &g[Line(l)];
            (0..g.columns())
                .map(|c| &row[Column(c)])
                .filter(|c| !c.flags.contains(Flags::WIDE_CHAR_SPACER))
                .map(|c| c.c)
                .collect::<String>()
                .trim_end()
                .into()
        })
        .collect()
}

/// codex の入力欄 (› で始まる行) が出ていて、メニューは出ていない。
/// codex は最初の指示まで SessionStart を送らないので、起動したかはこれで見る
pub(super) fn codex_prompt(term: &Term<Listener>) -> bool {
    let lines = screen_lines(term);
    parse_options(&lines).is_empty() && lines.iter().any(|l| l.trim_start().starts_with('›'))
}

/// 画面下側にある「1. ラベル」形式の行の並びを選択肢として返す。番号が 1 に戻ったら新しい並びとみなす。
pub(super) fn screen_options(term: &Term<Listener>) -> Vec<Value> {
    parse_options(&screen_lines(term))
}

pub(super) fn parse_options(lines: &[String]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for l in lines {
        let t = l.trim_start_matches([' ', '│', '┃', '|']);
        let selected = t.starts_with(MARKERS);
        let t = t.trim_start_matches(MARKERS).trim_start();
        let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
        let Some(label) = t[digits.len()..].strip_prefix(". ") else {
            continue;
        };
        let Ok(n) = digits.parse::<u32>() else {
            continue;
        };
        if n == 1 {
            out.clear();
        }
        let label = label.trim_end_matches([' ', '│', '┃', '|']).trim();
        out.push(json!({"n": n, "label": label, "selected": selected, "numbered": true}));
    }
    if out.is_empty() {
        out = unnumbered_options(lines);
    }
    out
}

pub(super) const MARKERS: [char; 4] = ['❯', '›', '>', '▶'];

/// 「❯ 選択中」の行と、その文字位置にそろえて上下に並ぶ行を選択肢とみなす (claude の信頼確認など)。
pub(super) fn unnumbered_options(lines: &[String]) -> Vec<Value> {
    let marker_text_col = |l: &str| -> Option<usize> {
        let lead = l.chars().take_while(|c| *c == ' ').count();
        let rest: String = l.chars().skip(lead).collect();
        let after = rest.strip_prefix(['❯', '›', '▶'])?;
        let gap = after.chars().take_while(|c| *c == ' ').count();
        (gap > 0 && !after.trim().is_empty()).then_some(lead + 1 + gap)
    };
    let sibling = |l: &str, col: usize| {
        let lead = l.chars().take_while(|c| *c == ' ').count();
        let first = l.chars().nth(lead);
        lead == col && first.is_some_and(|c| !('\u{2500}'..='\u{257F}').contains(&c))
    };
    let Some((idx, col)) = lines
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, l)| marker_text_col(l).map(|c| (i, c)))
    else {
        return Vec::new();
    };
    let mut lo = idx;
    while lo > 0 && sibling(&lines[lo - 1], col) {
        lo -= 1;
    }
    let mut hi = idx;
    while hi + 1 < lines.len() && sibling(&lines[hi + 1], col) {
        hi += 1;
    }
    if hi == lo {
        return Vec::new();
    }
    (lo..=hi)
        .map(|i| {
            let label: String = lines[i].chars().skip(col).collect();
            json!({"n": i - lo + 1, "label": label.trim(), "selected": i == idx, "numbered": false})
        })
        .collect()
}
