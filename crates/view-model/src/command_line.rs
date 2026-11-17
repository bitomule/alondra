/// What a shell command says in a list: its program and first arguments, a path by its last name and a URL by its
/// host, a value that could be a secret as «name=…», and nothing after a redirect. `None` when nothing readable is left.
pub fn readable_command(command: &str) -> Option<String> {
    const WORDS: usize = 4;
    const LONGEST_WORD: usize = 40;
    let words = command_segments(command).into_iter()
        .map(|segment| segment.into_iter().skip_while(|word| word.split_once('=').is_some_and(|(name, _)| is_variable(name))).collect::<Vec<_>>())
        .find(|segment| segment.first().is_some_and(|program| !matches!(program.as_str(), "cd" | "export" | "source" | "set" | "unset")))?;
    let mut shown = Vec::new();
    let mut hide_next = false;
    for (at, word) in words.iter().enumerate().take(WORDS) {
        let word = if at == 0 { word.rsplit('/').find(|part| !part.is_empty()).unwrap_or(word).to_owned() } else { word.clone() };
        let word = if std::mem::take(&mut hide_next) { "…".to_owned() } else { readable_word(&word) };
        hide_next = word.starts_with('-') && !word.contains('=') && names_a_secret(&word);
        shown.push(match word.char_indices().nth(LONGEST_WORD) {
            Some((end, _)) => format!("{}…", &word[..end]),
            None => word,
        });
    }
    let more = if words.len() > WORDS { " …" } else { "" };
    Some(format!("{}{more}", shown.join(" ")))
}

fn is_variable(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.starts_with(|c: char| c.is_ascii_digit())
}

fn names_a_secret(name: &str) -> bool {
    let name = name.to_lowercase();
    ["token", "secret", "password", "passwd", "credential", "auth", "apikey", "api-key", "api_key", "key"].iter().any(|word| name.contains(word))
}

/// One argument as it is shown: no secret, no home path, no credentials in a URL.
fn readable_word(word: &str) -> String {
    if let Some((name, value)) = word.split_once('=').filter(|(name, _)| name.starts_with('-') || is_variable(name)) {
        return if names_a_secret(name) { format!("{name}=…") } else { format!("{name}={}", readable_word(value)) };
    }
    if let Some((_, rest)) = word.split_once("://") {
        let host = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
        return host.split('/').next().unwrap_or(host).to_owned();
    }
    if (word.starts_with('/') && !word.starts_with("//")) || word.starts_with('~') {
        return word.rsplit('/').find(|part| !part.is_empty()).unwrap_or("~").to_owned();
    }
    word.to_owned()
}

/// The words of each command of a shell line, split at `&&`, `||`, `;` and `|`, each cut at its first redirect.
fn command_segments(line: &str) -> Vec<Vec<String>> {
    let mut segments = vec![Vec::new()];
    let (mut word, mut quote, mut redirected, mut started) = (String::new(), None::<char>, false, false);
    let mut chars = line.chars().peekable();
    let end_word = |segments: &mut Vec<Vec<String>>, word: &mut String, started: &mut bool, redirected: bool| {
        if *started && !redirected {
            segments.last_mut().expect("a segment").push(std::mem::take(word));
        }
        word.clear();
        *started = false;
    };
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => (quote, started) = (Some(c), true),
            (None, c) if c.is_whitespace() => end_word(&mut segments, &mut word, &mut started, redirected),
            (None, '>' | '<') => {
                if word.chars().all(|d| d.is_ascii_digit()) {
                    word.clear();
                    started = false;
                }
                end_word(&mut segments, &mut word, &mut started, redirected);
                redirected = true;
                if chars.peek() == Some(&'&') {
                    chars.next();
                }
            }
            (None, '&' | '|' | ';') => {
                end_word(&mut segments, &mut word, &mut started, redirected);
                if c == '&' && chars.peek().is_some_and(|next| *next == '>') {
                    redirected = true;
                    continue;
                }
                while chars.peek().is_some_and(|next| matches!(next, '&' | '|')) {
                    chars.next();
                }
                segments.push(Vec::new());
                redirected = false;
            }
            (None, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    end_word(&mut segments, &mut word, &mut started, redirected);
    segments
}
