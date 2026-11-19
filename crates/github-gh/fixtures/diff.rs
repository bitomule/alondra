use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (operation, number) = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["pr", "view", url, "--json", "headRefOid"] => ("head", url.rsplit('/').next().unwrap().parse::<u64>().unwrap()),
        ["pr", "diff", url] => ("diff", url.rsplit('/').next().unwrap().parse::<u64>().unwrap()),
        ["api", "graphql", "--hostname", "github.com", "-f", query, "-f", "owner=me", "-f", "name=app", "-F", number]
            if query.contains("viewerViewedState") => ("marks", number.strip_prefix("number=").unwrap().parse::<u64>().unwrap()),
        _ => panic!("unexpected arguments: {args:?}"),
    };
    if number == 99 {
        loop { std::thread::park(); }
    }
    if number == 3 { std::process::exit(1); }
    match operation {
        "head" => match number {
            4 => print!("not json"),
            5 => print!("{{\"headRefOid\":\"invalid\"}}"),
            _ => print!("{{\"headRefOid\":\"0123456789abcdef0123456789abcdef01234567\"}}"),
        },
        "diff" => {
            let bytes = match number {
                6 => vec![b'a'; 2 * 1024 * 1024],
                7 => vec![b'a'; 2 * 1024 * 1024 + 64],
                _ => b"diff --git a/src/a.rs b/src/a.rs\n+a\ndiff --git a/src/b.rs b/src/b.rs\n+b\n".to_vec(),
            };
            let _ = std::io::stdout().write_all(&bytes);
        }
        "marks" => {
            if number == 4 { print!("not json"); return; }
            let nodes = if number == 2 { "" } else {
                r#"{"path":"src/a.rs","viewerViewedState":"VIEWED"},{"path":"src/b.rs","viewerViewedState":"DISMISSED"},{"path":"src/c.rs","viewerViewedState":"UNVIEWED"}"#
            };
            print!("{{\"data\":{{\"repository\":{{\"pullRequest\":{{\"id\":\"PR_node\",\"headRefOid\":\"0123456789abcdef0123456789abcdef01234567\",\"files\":{{\"pageInfo\":{{\"hasNextPage\":false,\"endCursor\":null}},\"nodes\":[{nodes}]}}}}}}}}}}");
        }
        _ => unreachable!(),
    }
}
