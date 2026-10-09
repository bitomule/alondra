use clap::{Arg, ArgAction, Command};

/// clap writes "Usage", "Commands", "Arguments", "Options", "Print help" and the `help`
/// subcommand in English; this gives every command those texts in the user's language.
pub fn localized(cmd: Command) -> Command {
    let mut cmd = localize(cmd);
    cmd.build();
    localize_help_subcommands(&mut cmd);
    cmd
}

fn localize(cmd: Command) -> Command {
    cmd.help_template(template())
        .subcommand_help_heading(tr!("Commands", "Comandos"))
        .disable_help_flag(true)
        .arg(Arg::new("help").short('h').long("help").action(ArgAction::Help).help(tr!("Print help", "Muestra la ayuda")))
        .mut_args(|arg| {
            let heading = if arg.is_positional() { tr!("Arguments", "Argumentos") } else { tr!("Options", "Opciones") };
            arg.help_heading(heading)
        })
        .mut_subcommands(localize)
}

fn template() -> String {
    let usage = tr!("Usage", "Uso");
    format!("{{before-help}}{{about-with-newline}}\n{usage}: {{usage}}\n\n{{all-args}}{{after-help}}")
}

fn localize_help_subcommands(cmd: &mut Command) {
    if let Some(help) = cmd.find_subcommand_mut("help") {
        *help = std::mem::take(help)
            .about(tr!("Print this message or the help of the given subcommand(s)", "Muestra esta ayuda o la de los subcomandos indicados"))
            .help_template(template())
            .subcommand_help_heading(tr!("Commands", "Comandos"))
            .mut_args(|arg| {
                arg.help(tr!("Print help for the subcommand(s)", "Muestra la ayuda de los subcomandos")).help_heading(tr!("Arguments", "Argumentos"))
            });
    }
    for sub in cmd.get_subcommands_mut() {
        localize_help_subcommands(sub);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::CommandFactory;

    use super::*;
    use crate::i18n::{Lang, set_for_this_thread};

    fn cli(lang: Lang) -> Command {
        set_for_this_thread(lang);
        localized(crate::Cli::command())
    }

    /// Every visible command and argument, by path, with its help text.
    fn texts(cmd: &Command, path: &str, out: &mut Vec<(String, String)>) {
        if let Some(about) = cmd.get_about() {
            out.push((path.to_string(), about.to_string()));
        }
        for arg in cmd.get_arguments().filter(|a| !a.is_hide_set()) {
            if let Some(help) = arg.get_help() {
                out.push((format!("{path} {}", arg.get_id()), help.to_string()));
            }
        }
        for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
            texts(sub, &format!("{path} {}", sub.get_name()), out);
        }
    }

    #[test]
    fn every_visible_help_text_exists_in_both_languages_and_differs() {
        let (mut en, mut es) = (vec![], vec![]);
        texts(&cli(Lang::En), "alondra", &mut en);
        texts(&cli(Lang::Es), "alondra", &mut es);
        let paths = |t: &[(String, String)]| t.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>();
        assert_eq!(paths(&en), paths(&es));
        let untranslated: Vec<_> = en.iter().zip(&es).filter(|(a, b)| a.1 == b.1).map(|(a, _)| a.clone()).collect();
        assert!(untranslated.is_empty(), "help text that reads the same in English and Spanish: {untranslated:#?}");
    }

    #[test]
    fn spanish_help_has_no_english_headings_from_clap() {
        let mut cmd = cli(Lang::Es);
        let mut pages = vec![cmd.render_long_help().to_string()];
        let names: Vec<String> = cmd.get_subcommands().map(|s| s.get_name().to_string()).collect();
        for name in names {
            pages.push(cmd.find_subcommand_mut(&name).unwrap().render_long_help().to_string());
        }
        for page in &pages {
            for english in ["Usage:", "Commands:", "Arguments:", "Options:", "Print help", "Print this message"] {
                assert!(!page.contains(english), "{english:?} in:\n{page}");
            }
        }
    }

    fn snapshot(name: &str) -> PathBuf {
        match std::env::var("TEST_SRCDIR") {
            Ok(root) => PathBuf::from(root).join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into())).join("fixtures/cli").join(name),
            Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/cli").join(name),
        }
    }

    #[test]
    fn help_matches_its_snapshot_in_both_languages() {
        for (lang, code) in [(Lang::En, "en"), (Lang::Es, "es")] {
            let file = snapshot(&format!("help.{code}.txt"));
            let want = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            let got = cli(lang).render_help().to_string();
            assert_eq!(
                got.trim_end(),
                want.trim_end(),
                "alondra --help ({code}) changed; if intended: ALONDRA_LANG={code} alondra --help > fixtures/cli/help.{code}.txt"
            );
        }
    }
}
