//! agent-info must match reality: valid against the vendored framework
//! schema, every listed command routable, and the domain extras (models,
//! costs, breaking changes) in sync with the actual CLI surface.

mod common;
use common::{agent_info, suno, vendored_schema};

#[test]
fn manifest_validates_against_vendored_schema() {
    let schema = vendored_schema("agent-info.schema.json");
    let info = agent_info();
    if let Err(e) = jsonschema::validate(&schema, &info) {
        panic!("agent-info violates the vendored framework schema: {e}");
    }
}

#[test]
fn name_and_version_match_the_binary() {
    let info = agent_info();
    assert_eq!(info["name"], "suno");
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn every_listed_command_is_routable() {
    // Command keys may be multi-word ("config show"); each must accept
    // --help with exit 0 or the manifest is advertising a phantom command.
    let info = agent_info();
    for cmd in info["commands"].as_object().unwrap().keys() {
        let mut args: Vec<&str> = cmd.split_whitespace().collect();
        args.push("--help");
        let out = suno().args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "command listed in agent-info is not routable: {cmd}"
        );
    }
}

#[test]
fn advertised_options_exist_in_help() {
    // Assert the manifest's options against the real clap surface, not just
    // routability: every advertised long flag must appear in that command's
    // --help. Catches manifest drift (e.g. extend's new --force) and dead
    // flags the manifest never should have listed.
    let info = agent_info();
    for (cmd, spec) in info["commands"].as_object().unwrap() {
        let Some(options) = spec["options"].as_array() else {
            continue;
        };
        let mut args: Vec<&str> = cmd.split_whitespace().collect();
        args.push("--help");
        let out = suno().args(&args).output().unwrap();
        let help = String::from_utf8_lossy(&out.stdout);
        for opt in options {
            let name = opt["name"].as_str().unwrap();
            if name.starts_with("--") {
                assert!(
                    help.contains(name),
                    "manifest advertises {name} for `{cmd}` but --help does not show it"
                );
            }
        }
    }
}

#[test]
fn exit_codes_cover_0_to_4_and_5_is_gone() {
    let info = agent_info();
    let codes = info["exit_codes"].as_object().unwrap();
    for code in ["0", "1", "2", "3", "4"] {
        assert!(codes[code].is_string(), "must document exit code {code}");
    }
    // Code 5 (not found) was removed in 0.6.0 — not-found is now 3.
    assert!(
        !codes.contains_key("5"),
        "exit code 5 must not be documented"
    );
}

#[test]
fn models_map_includes_v4_5_all() {
    // The account-selectable "best free model" was missing from the enum
    // through 0.5.x; the manifest and --model must both know it.
    let info = agent_info();
    assert_eq!(info["models"]["v4.5-all"], "chirp-auk-turbo");

    let out = suno().args(["generate", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("v4.5-all"), "--model must accept v4.5-all");
}

#[test]
fn models_map_matches_model_flag_values() {
    // Every model in the manifest must be selectable via --model, and the
    // dead --variation flag must stay dead.
    let info = agent_info();
    let out = suno().args(["generate", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    for model in info["models"].as_object().unwrap().keys() {
        assert!(help.contains(model.as_str()), "--model missing {model}");
    }
    assert!(
        !help.contains("--variation"),
        "--variation was removed in 0.6.0"
    );
}

#[test]
fn current_v6_models_are_primary_and_legacy_models_are_labeled_retired() {
    let info = agent_info();
    assert_eq!(info["models"]["v6"], "chirp-hawk");
    assert_eq!(info["models"]["v6-wild"], "chirp-hawk-wild");
    assert_eq!(info["models"]["v6-mini"], "chirp-goose");
    assert_eq!(info["active_models"]["v6"], "chirp-hawk");
    assert_eq!(info["active_models"]["v6-wild"], "chirp-hawk-wild");
    assert_eq!(info["active_models"]["v6-mini"], "chirp-goose");
    assert_eq!(info["default_model"], "chirp-hawk (v6)");
    assert_eq!(info["remaster_models"]["v6"], "chirp-halibut");
    assert_eq!(info["default_remaster_model"], "chirp-halibut (v6)");

    let retired = info["retired_models"].as_array().unwrap();
    assert!(retired.contains(&serde_json::json!("v5.5")));
    assert!(!retired.contains(&serde_json::json!("v6")));
    assert!(
        info["retired_models_note"]
            .as_str()
            .unwrap()
            .contains("September 9")
    );
}

#[test]
fn generation_cost_uses_current_official_v6_guidance() {
    let info = agent_info();
    let cost = &info["generation_cost"];
    assert_eq!(cost["standard_v6"]["credits"], 10);
    assert_eq!(cost["standard_v6"]["outputs"], 2);
    assert_eq!(cost["standard_v6"]["as_of"], "2026-09-28");
    assert_eq!(
        cost["standard_v6"]["source"],
        "https://help.suno.com/en/articles/13924481"
    );
    assert!(cost["max_mode"].as_str().unwrap().contains("costs more"));
    assert!(
        cost["authority"]
            .as_str()
            .unwrap()
            .contains("authoritative")
    );
    assert!(
        !serde_json::to_string(cost).unwrap().contains("70"),
        "obsolete v5.5 cost guidance must not remain"
    );
}

#[test]
fn headless_generation_recovery_and_download_contracts_are_explicit() {
    let info = agent_info();

    for flag in ["--headless", "--no-browser"] {
        assert!(info["global_flags"][flag].is_object());
        let out = suno().arg("--help").output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains(flag));
    }
    assert!(
        info["global_flags"]["--headless"]["description"]
            .as_str()
            .unwrap()
            .contains("invisible")
    );
    assert!(
        info["global_flags"]["--no-browser"]["description"]
            .as_str()
            .unwrap()
            .contains("never access or launch a browser")
    );

    let option_names = |command: &str| {
        info["commands"][command]["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["name"].as_str().unwrap())
            .collect::<Vec<_>>()
    };
    for command in ["generate", "describe"] {
        let options = option_names(command);
        for flag in ["--dry-run", "--max-mode", "--request-id"] {
            assert!(options.contains(&flag), "{command} must advertise {flag}");
        }
    }
    for flag in ["--cookie-stdin", "--jwt-stdin"] {
        assert!(option_names("auth").contains(&flag));
    }
    for flag in ["--wait", "--download"] {
        assert!(option_names("status").contains(&flag));
    }
    assert_eq!(info["commands"]["jobs"]["options"][0]["default"], "10");
    assert!(
        info["commands"]["jobs"]["omitted_from_receipts"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("credentials"))
    );

    assert_eq!(
        info["commands"]["download"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == "--format")
            .unwrap()["values"],
        serde_json::json!(["mp3", "wav", "m4a", "mp4"])
    );
    assert_eq!(
        info["commands"]["download"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == "--source")
            .unwrap()["values"],
        serde_json::json!(["auto", "studio", "library", "playback"])
    );
    assert!(
        info["commands"]["download"]["transport"]
            .as_str()
            .unwrap()
            .contains("signed download-preparation APIs")
    );
    for command in ["generate", "describe", "cover", "remaster"] {
        let option = info["commands"][command]["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == "--download")
            .unwrap();
        assert!(
            option["description"]
                .as_str()
                .unwrap()
                .contains("implies --wait")
        );
    }
    assert!(
        info["workflows"]["generation_recovery"]["steps"][1]
            .as_str()
            .unwrap()
            .contains("status <id>... --wait --download DIR")
    );
    assert!(
        info["workflows"]["generation_recovery"]["steps"][2]
            .as_str()
            .unwrap()
            .contains("never resubmit")
    );
    assert!(
        info["commands"]["credits"]["data_fields"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("download_usage"))
    );
}

#[test]
fn breaking_changes_document_the_exit_code_remap() {
    let info = agent_info();
    let note = info["breaking_changes"]["0.6.0"]
        .as_str()
        .expect("0.6.0 breaking-change note must exist");
    assert!(
        note.contains("Exit codes"),
        "must mention the exit-code remap"
    );
}

#[test]
fn config_metadata_present() {
    let info = agent_info();
    assert!(info["config"]["path"].is_string());
    assert_eq!(info["config"]["env_prefix"], "SUNO_");
}
