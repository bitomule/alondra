use super::*;

fn shell(command: &str) -> Tool {
    Tool::Shell(command.into())
}

/// The cases of the token usage script's tests, which measured nodes before this did.
#[test]
fn steps_are_classified_as_the_token_usage_script_classified_them() {
    let cases = [
        (vec![shell("cd crates && grep -rn retry src | head")], StepKind::Look),
        (vec![shell("sed -n 1,40p a.rs; git status --short")], StepKind::Looks),
        (vec![shell("until grep -q done .scratch/log; do sleep 30; done")], StepKind::Poll),
        (vec![Tool::Wait], StepKind::Poll),
        (vec![shell("./scripts/test-all.sh")], StepKind::Other),
        (vec![Tool::Read], StepKind::Look),
    ];
    for (tools, kind) in cases {
        assert_eq!(classify(&tools), kind, "{tools:?}");
    }
}

#[test]
fn a_poll_is_a_sleep_a_loop_a_followed_log_or_a_watch_and_nothing_that_only_looks_like_one() {
    for command in ["sleep 5", "while true; do make; done", "tail -f log", "tail -nf log", "watch ls", "X=1 sleep  2"] {
        assert_eq!(classify(&[shell(command)]), StepKind::Poll, "{command}");
    }
    for command in ["sleeps 5", "sleep", "echo done", "tail -n 5 log", "stopwatch ls", "grep while a.rs"] {
        assert_ne!(classify(&[shell(command)]), StepKind::Poll, "{command}");
    }
}

#[test]
fn a_shell_call_only_looks_when_every_command_in_it_reads() {
    assert_eq!(classify(&[shell("ls; cargo build")]), StepKind::Other);
    assert_eq!(classify(&[shell("FOO=1 /usr/bin/git log | head")]), StepKind::Look);
    assert_eq!(classify(&[shell("cd x")]), StepKind::Other, "a lone cd runs nothing");
    assert_eq!(classify(&[]), StepKind::Text);
    assert_eq!(classify(&[Tool::Read, Tool::Read]), StepKind::Parallel);
    assert_eq!(classify(&[Tool::Read, Tool::Wait]), StepKind::Poll);
}

#[test]
fn a_call_split_over_several_messages_is_one_step_and_a_taken_one_is_not_counted_again() {
    let mut meter = StepMeter::default();
    meter.saw("m1", false, vec![Tool::Read]);
    meter.saw("m1", false, vec![shell("cargo test")]);
    meter.saw("m2", true, vec![shell("sleep 10")]);
    meter.saw("m3", false, vec![]);
    let counts = meter.take();
    assert_eq!(counts, StepCounts { steps: 2, subagent_steps: 1, looks: 0, polls: 1, tool_calls: 3 });
    meter.saw("m1", false, vec![Tool::Read]);
    meter.saw("m4", false, vec![Tool::Read]);
    assert_eq!(meter.take(), StepCounts { steps: 1, subagent_steps: 0, looks: 1, polls: 0, tool_calls: 1 });
    assert!(meter.take().is_empty());
}
