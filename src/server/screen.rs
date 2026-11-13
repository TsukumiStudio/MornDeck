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
pub(super) fn codex_prompt(lines: &[String]) -> bool {
    // 入力欄は画面の下端にある (下から 4 行以内)。› はメニューの選択中の行の印でもあるので、上のほうにあるものは入力欄とみなさない
    let bottom = lines
        .iter()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(4)
        .any(|l| l.trim_start().starts_with('›'));
    bottom && parse_options(lines).is_empty()
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
    // メニューは画面の下端にあり、後ろには操作の案内が少し続くだけ。送った指示の表示 (❯ で始まり、折り返しが同じ列に並ぶ) は
    // 後ろに回答や入力欄が続くので、メニューとして読まない
    if lines[hi + 1..]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .count()
        > 3
    {
        return Vec::new();
    }
    (lo..=hi)
        .map(|i| {
            let label: String = lines[i].chars().skip(col).collect();
            json!({"n": i - lo + 1, "label": label.trim(), "selected": i == idx, "numbered": false})
        })
        .collect()
}

/// codex の「Field 1/1」形式の確認 (Computer Use の許可など)。PermissionRequest のフックが来ないので画面で見る。
/// 「Field n/m」(未確定なら後ろに「 (1 required unanswered)」) の行の下にカーソル付きの選択肢があり、それが画面の下端にあれば、
/// Field の次の行 (質問) と、選択肢までの行 (App: など) を返す
pub(super) fn field_prompt(lines: &[String]) -> Option<(String, Vec<String>)> {
    let field = |t: &str| {
        t.strip_prefix("Field ")
            .and_then(|r| r.split_once('/'))
            .is_some_and(|(a, b)| {
                let b = b.split_once(" (").map_or(b, |(n, _)| n);
                [a, b]
                    .iter()
                    .all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
            })
    };
    let i = lines.iter().rposition(|l| field(l.trim()))?;
    if !parse_options(&lines[i + 1..])
        .iter()
        .any(|o| o["selected"] == true)
    {
        return None;
    }
    let rest: Vec<&str> = lines[i + 1..]
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let option_line = |l: &str| {
        let t = l.trim_start_matches(MARKERS).trim_start();
        let d = t.chars().take_while(char::is_ascii_digit).count();
        d > 0 && t[d..].starts_with(". ")
    };
    let first = rest.iter().position(|l| option_line(l))?;
    let last = rest.iter().rposition(|l| option_line(l))?;
    // 質問と補足は折り返しても 10 行まで。選択肢の後ろは操作の案内だけ (画面の下端にある確認)
    if first == 0 || first > 10 || rest.len() - last - 1 > 3 {
        return None;
    }
    let details = rest[1..first].iter().map(|l| l.to_string()).collect();
    Some((rest[0].to_string(), details))
}
