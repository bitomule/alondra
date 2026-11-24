use super::*;
use crate::accounts::{AccountChange, AccountId, AccountKey, AccountsConfig, Provider, provider_folder};
use serde_json::json;

fn inherited() -> BTreeMap<String, String> {
    [
        ("PATH", "/usr/bin:/bin"), ("USER", "ana"), ("LOGNAME", "ana"), ("LANG", "es_ES.UTF-8"),
        ("GEMINI_API_KEY", "inherited-key"), ("GOOGLE_API_KEY", "inherited-cloud-key"), ("GOOGLE_APPLICATION_CREDENTIALS", "/Users/ana/sa.json"),
        ("GOOGLE_CLOUD_PROJECT", "other-project"), ("GOOGLE_CLOUD_LOCATION", "us-east1"), ("GOOGLE_CLOUD_QUOTA_PROJECT", "other-quota"),
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"), ("AGY_ADC_AUTH", "true"), ("GOOGLE_GEMINI_BASE_URL", "https://example.org"),
        ("GEMINI_HOME", "/Users/ana/elsewhere"), ("OPENAI_API_KEY", "not-for-agy"), ("CLAUDE_CONFIG_DIR", "/Users/ana/.claude-work"),
    ].into_iter().map(|(key, value)| (key.to_owned(), value.to_owned())).collect()
}

#[test]
fn every_launch_drops_the_inherited_credentials_and_sets_only_the_accounts_own() {
    let folder = Path::new("/Users/ana/.alondra/provider-accounts/antigravity/work");
    for access in Access::ALL {
        let settings = Settings { access, project: access.takes_project().then(|| "demo-project".into()), location: access.takes_project().then(|| "europe-west1".into()), credentials_file: None };
        for (folder, key) in [(None, None), (Some(folder), None), (Some(folder), Some("own-key"))] {
            let launch = launch(&inherited(), Path::new("/Users/ana"), &settings, folder, key, true);
            let env = &launch.environment;
            for value in ["inherited-key", "inherited-cloud-key", "/Users/ana/sa.json", "other-project", "other-quota", "us-east1", "https://example.org", "/Users/ana/elsewhere", "not-for-agy", "/Users/ana/.claude-work"] {
                assert!(!env.values().any(|set| set == value), "{access:?} inherited {value}");
            }
            assert_eq!(env["AGY_CLI_DISABLE_AUTO_UPDATE"], "1");
            assert_eq!(env["HOME"], "/Users/ana");
            assert_eq!(env["LANG"], "es_ES.UTF-8");
            assert_eq!(env.get("GEMINI_HOME").map(String::as_str), folder.and_then(Path::to_str));
            assert_eq!(launch.args.is_empty(), folder.is_none());
            let file_storage = folder.is_some() && access.signs_in_with_google();
            assert_eq!(env.get("AGY_ACP_FORCE_FILE_STORAGE").is_some(), file_storage, "{access:?}");
            assert_eq!(env.get("SSH_CONNECTION").map(String::as_str), file_storage.then_some(FILE_STORAGE_SSH_CONNECTION));
            assert_eq!(env.get("GEMINI_API_KEY").map(String::as_str), (access == Access::ApiKey).then_some(key).flatten());
            assert_eq!(env.get("GOOGLE_API_KEY").map(String::as_str), (access == Access::CloudProject).then_some(key).flatten());
            assert_eq!(env.get("AGY_ADC_AUTH").is_some(), access == Access::CloudProject && key.is_none());
            assert_eq!(env.get("GOOGLE_CLOUD_QUOTA_PROJECT").is_some(), access.takes_project());
            assert_eq!(env.get("GOOGLE_CLOUD_LOCATION").is_some(), access.takes_project());
        }
    }
}

#[test]
fn a_cloud_account_names_its_service_account_file_only_with_adc() {
    let settings = Settings { access: Access::CloudProject, project: Some("demo".into()), location: None, credentials_file: Some("/Users/ana/keys/sa.json".into()) };
    let adc = launch(&BTreeMap::new(), Path::new("/Users/ana"), &settings, None, None, false);
    assert_eq!(adc.environment["GOOGLE_APPLICATION_CREDENTIALS"], "/Users/ana/keys/sa.json");
    let keyed = launch(&BTreeMap::new(), Path::new("/Users/ana"), &settings, None, Some("cloud-key"), false);
    assert!(!keyed.environment.contains_key("GOOGLE_APPLICATION_CREDENTIALS"));
    let usage = launch(&BTreeMap::new(), Path::new("/Users/ana"), &settings, Some(Path::new("/tmp/a")), None, false);
    assert!(usage.args.is_empty(), "only agy acp takes --gemini-dir");
}

#[test]
fn each_access_has_its_sign_in_command() {
    let google = Settings::of(Access::Google);
    assert_eq!(sign_in_command(&google, None).unwrap(), "agy acp --login");
    assert_eq!(
        sign_in_command(&google, Some("~/.alondra/provider-accounts/antigravity/work")).unwrap(),
        "GEMINI_HOME=~/.alondra/provider-accounts/antigravity/work AGY_ACP_FORCE_FILE_STORAGE=1 SSH_CONNECTION='127.0.0.1 0 127.0.0.1 0' agy acp --gemini-dir ~/.alondra/provider-accounts/antigravity/work --login"
    );
    assert!(sign_in_command(&Settings::of(Access::Enterprise), Some("/Users/ana/My Folder")).unwrap().contains("--gemini-dir '/Users/ana/My Folder' --login"));
    assert_eq!(sign_in_command(&Settings::of(Access::ApiKey), Some("/x")), None);
    let cloud = Settings { access: Access::CloudProject, project: Some("demo-project".into()), ..Settings::default() };
    assert_eq!(sign_in_command(&cloud, None).unwrap(), "gcloud auth application-default login --project demo-project");
    assert_eq!(crate::accounts::sign_in_command(Provider::Antigravity, None), "agy acp --login");
}

#[test]
fn settings_refuse_what_their_access_does_not_take() {
    assert!(Settings::of(Access::Google).check().is_ok());
    assert!(Settings { project: Some("demo".into()), ..Settings::of(Access::Google) }.check().is_err());
    assert!(Settings { project: Some("demo".into()), ..Settings::of(Access::ApiKey) }.check().is_err());
    assert!(Settings::of(Access::CloudProject).check().is_err(), "a cloud account needs its project");
    assert!(Settings { project: Some("demo; rm".into()), ..Settings::of(Access::CloudProject) }.check().is_err());
    assert!(Settings { project: Some("demo".into()), credentials_file: Some("relative.json".into()), ..Settings::of(Access::CloudProject) }.check().is_err());
    assert!(Settings { credentials_file: Some("/Users/ana/sa.json".into()), ..Settings::of(Access::Enterprise) }.check().is_err());
    assert!(Settings { project: Some("demo".into()), location: Some("global".into()), credentials_file: Some("/Users/ana/sa.json".into()), ..Settings::of(Access::CloudProject) }.check().is_ok());
}

#[test]
fn billing_follows_the_access_unless_the_user_sets_it() {
    for (access, paid) in [(Access::Google, false), (Access::Enterprise, false), (Access::ApiKey, true), (Access::CloudProject, true)] {
        assert_eq!(access.default_billing() == crate::accounts::AccountBilling::PayPerUse, paid, "{access:?}");
        assert_eq!(access.reads_usage(), !paid);
    }
}

#[test]
fn the_section_decodes_without_antigravity_and_round_trips_with_it() {
    let old: AccountsConfig = toml::from_str::<toml::Table>("[accounts]\nconnected = [\"claude\"]\n").unwrap()["accounts"].clone().try_into().unwrap();
    assert!(old.antigravity.is_empty());
    let text = "[accounts]\nconnected = [\"antigravity\"]\n\n[[accounts.antigravity]]\nid = \"cloud\"\nname = \"Cloud\"\naccess = \"cloud_project\"\nproject = \"demo-project\"\nlocation = \"europe-west1\"\ncredentials_file = \"/Users/ana/sa.json\"\n\n[[accounts.antigravity]]\nid = \"studio\"\nname = \"Studio\"\naccess = \"api_key\"\n";
    let config = crate::options::OptionsFile::parse(text).unwrap().accounts;
    config.check().unwrap();
    let cloud = &config.antigravity[0];
    assert_eq!(cloud.settings(), Settings { access: Access::CloudProject, project: Some("demo-project".into()), location: Some("europe-west1".into()), credentials_file: Some("/Users/ana/sa.json".into()) });
    assert_eq!(cloud.account().billing, crate::accounts::AccountBilling::PayPerUse);
    assert_eq!(config.accounts(Provider::Antigravity).iter().map(|account| account.id.as_str()).collect::<Vec<_>>(), ["default", "cloud", "studio"]);
    let again = crate::options::OptionsFile::parse(&config.to_toml()).unwrap().accounts;
    assert_eq!(again, config);
    assert!(crate::options::OptionsFile::parse(&text.replace("access = \"api_key\"", "access = \"api_key\"\nkey = \"secret\"")).is_err(), "a key never goes in options.toml");
}

#[test]
fn adding_an_account_keeps_its_access_and_an_api_key_one_is_always_named() {
    let config = AccountsConfig::fresh();
    let studio = config.changed(Provider::Antigravity, &AccountChange::Add { name: "Studio".into(), antigravity: Some(Settings::of(Access::ApiKey)) }).unwrap();
    let added = studio.antigravity.iter().find(|account| account.name == "Studio").unwrap();
    assert_eq!(added.id.as_str(), "studio");
    assert_eq!(added.access, Access::ApiKey);
    assert_eq!(added.billing, None, "the billing its access implies is not written out");
    let renamed = studio.changed(Provider::Antigravity, &AccountChange::Rename { account: added.id.clone(), name: "AI Studio".into() }).unwrap();
    assert_eq!(renamed.antigravity.iter().find(|account| account.id == added.id).unwrap().settings(), Settings::of(Access::ApiKey), "a change keeps the access");
    let google = AccountsConfig::fresh().changed(Provider::Antigravity, &AccountChange::Add { name: "Personal".into(), antigravity: None }).unwrap();
    assert!(google.antigravity[0].id.is_default());
    assert!(AccountsConfig::fresh().changed(Provider::Codex, &AccountChange::Add { name: "x".into(), antigravity: Some(Settings::default()) }).is_err());
    let mut default_key = AccountsConfig::fresh();
    default_key.connect(Provider::Antigravity);
    default_key.antigravity.push(crate::accounts::AntigravityAccount::new(crate::accounts::ProviderAccount::implicit_default(), Settings::of(Access::ApiKey)));
    assert!(default_key.check().is_err(), "the default account is agy's own sign-in");
}

#[test]
fn a_named_account_has_a_private_folder_of_its_own() {
    let home = Path::new("/Users/ana/.alondra");
    let key = AccountKey::new(Provider::Antigravity, AccountId::parse("work").unwrap());
    assert_eq!(provider_folder(home, &key).unwrap(), home.join("provider-accounts/antigravity/work"));
    assert_eq!(provider_folder(home, &AccountKey::native(Provider::Antigravity)), None);
    assert_eq!(Provider::from_driver("antigravity-acp"), Some(Provider::Antigravity));
    assert_eq!(Provider::parse("antigravity"), Ok(Provider::Antigravity));
    assert_eq!(AccountKey::parse_wire("antigravity/work").unwrap(), key);
}

#[test]
fn versions_below_the_minimum_are_refused() {
    assert_eq!(Version::parse("1.5.14\n"), Some(Version(1, 5, 14)));
    assert_eq!(Version::parse("agy version v1.5.12-beta"), Some(Version(1, 5, 12)));
    assert!(Version::parse("1.5.12").unwrap().supported());
    assert!(!Version::parse("1.1.22").unwrap().supported());
    assert!(!Version::parse("1.4.99").unwrap().supported());
    assert!(Version::parse("2.0.0").unwrap().supported());
    assert_eq!(Version::parse("no version"), None);
}

#[test]
fn a_model_belongs_to_the_family_its_id_names() {
    assert_eq!(model_family("gemini-4-neon"), "gemini-4");
    assert_eq!(model_family("Gemini-3.8-Flash"), "gemini-3.8");
    assert_eq!(model_family("gemini-4"), "gemini-4");
    assert_eq!(model_family("claude-opus"), "claude-opus");
    assert_eq!(crate::model_table::measured_key(Provider::Antigravity, "antigravity/gemini-4-carbon"), "gemini-4");
}

#[test]
fn thinking_levels_map_to_efforts_and_back() {
    assert_eq!(effort_of("extra"), "xhigh");
    assert_eq!(effort_of("xhigh"), "xhigh");
    assert_eq!(effort_of("ultra-deep"), "ultra-deep", "an unknown level keeps its own id");
    let offered = vec!["low".to_owned(), "medium".into(), "extra".into()];
    assert_eq!(level_for("xhigh", &offered), Some("extra"));
    assert_eq!(level_for("medium", &offered), Some("medium"));
    assert_eq!(level_for("max", &offered), None);
}

#[test]
fn the_catalog_reads_the_sessions_select_options() {
    let options = json!([
        {"id":"model","category":"model","type":"select","currentValue":"gemini-4-carbon","options":[{"value":"gemini-4-neon","name":"Gemini 4 Neon"},{"value":"gemini-4-carbon","name":"Gemini 4 Carbon"},{"value":"bad\u{7}","name":"x"}]},
        {"id":"thought_level","category":"thought_level","type":"select","currentValue":"medium","options":[{"value":"low","name":"Low"},{"value":"medium","name":"Medium"},{"value":"extra","name":"Extra"}]}
    ]);
    let models = select_option(&options, "model").unwrap();
    assert_eq!(models.current.as_deref(), Some("gemini-4-carbon"));
    assert_eq!(models.values, [("gemini-4-neon".to_owned(), "Gemini 4 Neon".to_owned()), ("gemini-4-carbon".into(), "Gemini 4 Carbon".into())]);
    let levels = select_option(&options, "thought_level").unwrap();
    let model = catalog_model("gemini-4-carbon", "Gemini 4 Carbon", &levels, true);
    assert_eq!(model.reasoning_efforts, ["low", "medium", "xhigh"]);
    assert_eq!(model.default_reasoning_effort.as_deref(), Some("medium"));
    assert!(model.is_default);
    assert!(select_option(&options, "mode").is_none());
}

#[test]
fn usage_has_one_window_per_pool_and_the_account_stops_once_every_pool_ran_out() {
    use crate::usage::record;
    let report = |left_gemini: f64, left_claude: f64| json!({"groups": [{"name": "All Models", "buckets": [
        {"id": "gemini-4-neon-high", "remaining_fraction": left_gemini, "reset_time": "2030-01-01T00:00:00Z"},
        {"id": "gemini-4-neon-low", "remaining_fraction": left_gemini, "reset_time": "2030-01-01T00:00:00Z"},
        {"id": "claude-opus-5.5-max", "remaining_fraction": left_claude, "reset_time": "2030-01-02T00:00:00Z"}]}]});
    let reading = parse_usage(&report(0.25, 0.0)).unwrap();
    let names: Vec<_> = reading.windows.iter().map(|window| window.kind.name().to_owned()).collect();
    assert_eq!(names, ["antigravity:all-models:gemini-4-neon-high", "antigravity:all-models:claude-opus-5.5-max"]);
    assert_eq!(reading.windows[0].kind.antigravity_pool(), Some("gemini-4-neon-high"));
    assert_eq!(reading.windows[0].used, crate::usage::Share::of(0.75));
    assert!(reading.windows[1].exhausted);
    assert_eq!(reading.windows[0].resets_at, parse_utc("2030-01-01T00:00:00Z"));
    let full = parse_usage(&json!({"groups": [{"name": "All Models", "buckets": [{"id": "a", "remaining_fraction": 1, "reset_time": "1970-01-01T00:00:00Z"}]}]})).unwrap();
    assert_eq!(full.windows[0].resets_at, None, "the epoch means no reset is pending");
    let account = AccountId::default_account();
    let some = record(None, Provider::Antigravity, &account, &reading, 0, "2026-11-22");
    assert!(!some.plan_exhausted(0), "Gemini still has room");
    let spent = record(Some(&some), Provider::Antigravity, &account, &parse_usage(&report(0.0, 0.0)).unwrap(), 1, "2026-11-22");
    assert!(spent.plan_exhausted(1));
    assert_eq!(spent.plan_exhausted_until(1), parse_utc("2030-01-01T00:00:00Z"), "the account is back with the first pool that resets");
    let status = crate::accounts::AccountStatus { provider: Provider::Antigravity, id: account.clone(), name: "Personal".into(), state: crate::accounts::AccountState::SignedIn,
        native_identity: None, email: None, plan: None, billing: crate::accounts::AccountBilling::default(), sign_ins: 0, profile: None, antigravity: None };
    assert_eq!(crate::usage::margin_returns_at(Provider::Antigravity, std::slice::from_ref(&status), std::slice::from_ref(&spent), 1), parse_utc("2030-01-01T00:00:00Z"));
    assert_eq!(crate::usage::margin_returns_at(Provider::Antigravity, std::slice::from_ref(&status), std::slice::from_ref(&some), 1), None);
}

#[test]
fn utc_times_read_as_milliseconds() {
    assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_utc("2026-11-22T10:00:00.5Z"), Some(1_795_341_600_500));
    assert_eq!(parse_utc("2026-11-22T10:00:00.5+00:00"), Some(1_795_341_600_500), "UTC written as an offset is the same instant");
    assert_eq!(parse_utc("2026-13-01T00:00:00Z"), None);
    assert_eq!(parse_utc("2026-11-22 10:00:00"), None);
}

#[test]
fn a_launch_never_prints_its_key() {
    let launch = launch(&BTreeMap::new(), Path::new("/Users/ana"), &Settings::of(Access::ApiKey), Some(Path::new("/tmp/a")), Some("AIza-secret"), true);
    let printed = format!("{launch:?}");
    assert!(!printed.contains("AIza-secret") && printed.contains("GEMINI_API_KEY"), "{printed}");
}

#[test]
fn utc_times_out_of_range_or_signed_are_refused() {
    assert_eq!(parse_utc("2026-11-22T-1:00:00Z"), None);
    assert_eq!(parse_utc("99999999999999-01-01T00:00:00Z"), None);
    assert_eq!(parse_utc("1969-12-31T23:59:59Z"), None);
}

#[test]
fn pools_whose_names_collide_keep_a_window_each() {
    let reading = parse_usage(&json!({"groups": [{"name": "All Models", "buckets": [
        {"id": "claude-opus-4-5-thinking-max", "remaining_fraction": 0.5, "reset_time": "2030-01-01T00:00:00Z"},
        {"id": "claude-opus-4-5-thinking-low", "remaining_fraction": 0.0, "reset_time": "2030-01-02T00:00:00Z"}]}]})).unwrap();
    assert_eq!(reading.windows.len(), 2, "{:?}", reading.windows);
    assert!(reading.windows.iter().any(|window| window.exhausted));
}

#[test]
fn a_pool_whose_reset_passed_has_room_again() {
    let reading = parse_usage(&json!({"groups": [{"name": "All Models", "buckets": [
        {"id": "gemini-4-neon-high", "remaining_fraction": 0.7, "reset_time": "2030-01-01T00:00:00Z"},
        {"id": "claude-opus-max", "remaining_fraction": 0, "reset_time": "2030-01-02T00:00:00Z"}]}]})).unwrap();
    let measured = crate::usage::record(None, Provider::Antigravity, &AccountId::default_account(), &reading, 0, "2026-11-22");
    let after_gemini_reset = parse_utc("2030-01-01T12:00:00Z").unwrap();
    assert!(!measured.plan_exhausted(after_gemini_reset), "Gemini's pool reset, so the account has room even with Claude's spent");
}

#[test]
fn the_account_that_stays_on_runs_nodes_unless_antigravity_is_alone() {
    let mut alone = AccountsConfig::fresh();
    alone.connect(Provider::Antigravity);
    assert!(alone.any_enabled_globally(), "Antigravity connected alone keeps its own account on");
    let mut both = alone.clone();
    both.connect(Provider::Claude);
    assert!(both.any_enabled_globally());
    let claude_off = both.changed(Provider::Claude, &AccountChange::Disable { account: AccountId::default_account() }).unwrap();
    assert!(!claude_off.any_enabled_globally(), "an Antigravity account does not stand for the last Claude one");
}
