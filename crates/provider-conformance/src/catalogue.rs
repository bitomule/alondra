//! The scenarios and the divergences between adapters known today.
use crate::Provider;
use alondra_provider_contract::Capability;

pub struct Scenario {
    pub name: &'static str,
    pub needs: &'static [Capability],
    /// The neutral facts every provider must produce, in order.
    pub expected: &'static [&'static str],
    claude: Option<&'static str>,
    codex: Option<&'static str>,
    antigravity: Option<&'static str>,
}
impl Scenario {
    pub fn transcript(&self, provider: Provider) -> Option<&'static str> {
        match provider {
            Provider::Claude => self.claude,
            Provider::Codex => self.codex,
            Provider::Antigravity => self.antigravity,
        }
    }
}

/// A difference between what a provider produces and what is expected, known and explained.
pub struct KnownDivergence {
    pub scenario: &'static str,
    pub provider: Provider,
    pub cause: &'static str,
    /// The neutral facts the provider produces instead, exactly.
    pub actual: &'static [&'static str],
}

macro_rules! transcripts {
    ($name:literal) => {
        (
            Some(include_str!(concat!("../transcripts/claude/", $name, ".jsonl"))),
            Some(include_str!(concat!("../transcripts/codex/", $name, ".jsonl"))),
            Some(include_str!(concat!("../transcripts/antigravity/", $name, ".jsonl"))),
        )
    };
}

pub(crate) const fn scenario(
    name: &'static str,
    needs: &'static [Capability],
    (claude, codex, antigravity): (Option<&'static str>, Option<&'static str>, Option<&'static str>),
    expected: &'static [&'static str],
) -> Scenario {
    Scenario { name, needs, expected, claude, codex, antigravity }
}

pub const SCENARIOS: &[Scenario] = &[
    scenario("simple-turn", &[Capability::OfferInput], transcripts!("simple-turn"), &[
        "Available",
        "InputHeld(input-1)",
        "TurnStarted(T1, input-1)",
        "ModelAnswered",
        "TurnEnded(T1, Completed)",
        "Reconciled(idle, live=[])",
    ]),
    scenario("several-turns", &[Capability::OfferInput], transcripts!("several-turns"), &[
        "Available",
        "InputHeld(input-1)",
        "TurnStarted(T1, input-1)",
        "ModelAnswered",
        "TurnEnded(T1, Completed)",
        "Reconciled(idle, live=[])",
        "InputHeld(input-2)",
        "TurnStarted(T2, input-2)",
        "ModelAnswered",
        "TurnEnded(T2, Completed)",
        "Reconciled(idle, live=[])",
    ]),
    scenario("input-during-turn", &[Capability::OfferInput], transcripts!("input-during-turn"), &[
        "Available",
        "InputHeld(input-1)",
        "TurnStarted(T1, input-1)",
        "InputHeld(input-2)",
        "ModelAnswered",
        "TurnEnded(T1, Completed)",
        "TurnStarted(T2, input-2)",
        "ModelAnswered",
        "TurnEnded(T2, Completed)",
        "Reconciled(idle, live=[])",
    ]),
    scenario("interrupt", &[Capability::OfferInput, Capability::Interrupt], transcripts!("interrupt"), &[
        "Available",
        "InputHeld(input-1)",
        "TurnStarted(T1, input-1)",
        "ModelAnswered",
        "TurnEnded(T1, Failed(interrupted))",
        "Reconciled(idle, live=[])",
    ]),
    scenario("process-dies-then-resume", &[Capability::OfferInput, Capability::DurableResume], transcripts!("process-dies-then-resume"), &[
        "Available",
        "InputHeld(input-1)",
        "TurnStarted(T1, input-1)",
        "HostEnded",
        "InputHeld(input-2)",
        "TurnStarted(T2, input-2)",
        "ModelAnswered",
        "TurnEnded(T2, Completed)",
        "Reconciled(idle, live=[])",
    ]),
    // Regression of 04f2a705e: an input the provider finished before its turn's result named it
    // again was taken again, and the next turn the provider opened by itself was lost whole.
    scenario("finished-input-not-taken-again", &[Capability::OfferInput], transcripts!("finished-input-not-taken-again"), &[
        "Available",
        "InputHeld(input-1)",
        "TurnStarted(T1, input-1)",
        "ModelAnswered",
        "TurnEnded(T1, Completed)",
        "Reconciled(idle, live=[])",
        "TurnUnprompted(T2)",
        "ModelAnswered",
        "TurnEnded(T2, Completed)",
        "Reconciled(idle, live=[])",
    ]),
    // Regression of d41881d84 (#185): an input counts as taken only when the provider names it;
    // a turn that ends without naming it drops it.
    scenario("consumed-input-named-by-provider", &[Capability::OfferInput], transcripts!("consumed-input-named-by-provider"), &[
        "Available",
        "InputHeld(input-1)",
        "InputDropped(input-1)",
        "Reconciled(idle, live=[])",
    ]),
    scenario("decisions", &[Capability::OfferInput, Capability::Decisions], transcripts!("decisions"), &[
        "Available",
        "InputHeld(input-1)",
        "TurnStarted(T1, input-1)",
        "ModelAnswered",
        "Asked(D1, E1, Permission)",
        "Settled(E1, answered, continued)",
        "Asked(D2, E2, Question)",
        "Settled(E2, unanswered, continued)",
        "TurnEnded(T1, Completed)",
        "Reconciled(idle, live=[])",
    ]),
    // Regressions of 7a23ad13b (#10) and 3f4775ac9 (#145): a turn the usage limit ends says so,
    // with when the limit resets.
    scenario("usage-limit-reset", &[Capability::OfferInput], transcripts!("usage-limit-reset"), &[
        "Available",
        "InputHeld(input-1)",
        "TurnStarted(T1, input-1)",
        "Usage(session used=10000 exhausted resets=1790202000000)",
        "TurnEnded(T1, ApiFailed(rate_limit, status=429, resets=1790202000))",
        "Reconciled(idle, live=[])",
    ]),
];

pub const DIVERGENCES: &[KnownDivergence] = &[
    KnownDivergence {
        scenario: "input-during-turn",
        provider: Provider::Codex,
        cause: "the Codex host keeps an input sent during a turn in its own queue and starts it only once the turn is over \
                (agent-host/codex-session.ts pump), so Codex names it held only then, after the thread went idle",
        actual: &[
            "Available",
            "InputHeld(input-1)",
            "TurnStarted(T1, input-1)",
            "ModelAnswered",
            "TurnEnded(T1, Completed)",
            "Reconciled(idle, live=[])",
            "InputHeld(input-2)",
            "TurnStarted(T2, input-2)",
            "ModelAnswered",
            "TurnEnded(T2, Completed)",
            "Reconciled(idle, live=[])",
        ],
    },
    KnownDivergence {
        scenario: "interrupt",
        provider: Provider::Claude,
        cause: "the contract has no interrupted outcome: Claude ends the turn with its result's subtype error_during_execution, \
                and the domain tells the interrupt only from its own record of asking for it (core lifecycle.rs observed_turn_end)",
        actual: &[
            "Available",
            "InputHeld(input-1)",
            "TurnStarted(T1, input-1)",
            "ModelAnswered",
            "TurnEnded(T1, Failed(error_during_execution))",
            "Reconciled(idle, live=[])",
        ],
    },
    KnownDivergence {
        scenario: "consumed-input-named-by-provider",
        provider: Provider::Codex,
        cause: "Codex says the thread is idle before the turn/completed of the turn it started for the input; with no turn \
                open the normalizer reconciles at once (provider-codex normalizer.rs thread/status/changed), before the drop",
        actual: &[
            "Available",
            "InputHeld(input-1)",
            "Reconciled(idle, live=[])",
            "InputDropped(input-1)",
        ],
    },
    KnownDivergence {
        scenario: "usage-limit-reset",
        provider: Provider::Codex,
        cause: "the Codex normalizer disguises usageLimitExceeded as Claude's error (rate_limit, 429) with no reset \
                (provider-codex normalizer.rs failed_outcome) and reads no usage window from the thread: Codex's limits \
                come only from polling account/rateLimits/read",
        actual: &[
            "Available",
            "InputHeld(input-1)",
            "TurnStarted(T1, input-1)",
            "TurnEnded(T1, ApiFailed(rate_limit, status=429, resets=none))",
            "Reconciled(idle, live=[])",
        ],
    },
    KnownDivergence {
        scenario: "input-during-turn",
        provider: Provider::Antigravity,
        cause: "the Antigravity host keeps an input sent during a turn in its own queue and sends its session/prompt only once \
                the prompt before it is answered (agent-host/antigravity-session.ts), so the input is held only then, after the \
                turn ended",
        actual: &[
            "Available",
            "InputHeld(input-1)",
            "TurnStarted(T1, input-1)",
            "ModelAnswered",
            "TurnEnded(T1, Completed)",
            "Reconciled(idle, live=[])",
            "InputHeld(input-2)",
            "TurnStarted(T2, input-2)",
            "ModelAnswered",
            "TurnEnded(T2, Completed)",
            "Reconciled(idle, live=[])",
        ],
    },
    KnownDivergence {
        scenario: "usage-limit-reset",
        provider: Provider::Antigravity,
        cause: "agy reports a failed upstream call, a used-up quota included, as an agent message and answers the prompt end_turn \
                (measured on agy 1.5.14), and ACP carries no usage window: the account's windows come from the usage reading",
        actual: &[
            "Available",
            "InputHeld(input-1)",
            "TurnStarted(T1, input-1)",
            "ModelAnswered",
            "TurnEnded(T1, Completed)",
            "Reconciled(idle, live=[])",
        ],
    },
];
