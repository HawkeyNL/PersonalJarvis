//! The root-owned paper profile. Parsed and validated in `jarvis-onchain`,
//! shared with `jarvis memecoin init-profile`, so the rules exist once.

pub use jarvis_onchain::profile::{is_mint, PaperProfile};

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use jarvis_onchain::profile::STRATEGY_VERSION_FLOOR;
    use time::Duration;

    pub(crate) const MINT_A: &str = "So11111111111111111111111111111111111111112";
    pub(crate) const MINT_B: &str = "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263";

    /// Example values for tests only, not advice.
    pub(crate) fn fixture_json() -> serde_json::Value {
        serde_json::json!({
            "watchlist": [MINT_A, MINT_B],
            "goal_multiple": "2",
            "stake_usd": "250",
            "max_position_usd": "25",
            "max_open_positions": 3,
            "filter": {
                "min_liquidity_usd": "20000",
                "min_pool_age_secs": 3600,
                "max_top_holders_share": "0.3",
                "max_sell_impact_bps": 300,
                "allowed_extensions": ["metadata_pointer", "token_metadata"],
                "max_snapshot_age_secs": 60,
                "min_lp_lock_remaining_secs": 2592000
            },
            "strategy": {
                "version": 3, "min_buy_ratio_bps": 6000, "min_momentum_bps": 500,
                "max_momentum_bps": 5000, "position_share_bps": 10000,
                "max_liquidity_share_bps": 100, "take_profit_bps": 5000,
                "stop_loss_bps": 2500, "liquidity_drop_exit_bps": 4000,
                "max_hold_secs": 14400, "entry_slippage_bps": 150,
                "exit_slippage_bps": 800, "intent_ttl_secs": 20
            },
            "costs": {
                "dex_fee_bps": 25, "network_fee_usd": "0.001",
                "priority_fee_usd": "0.01", "failure_rate": "0.1"
            }
        })
    }

    pub(crate) fn fixture() -> PaperProfile {
        PaperProfile::parse(&fixture_json().to_string()).unwrap()
    }

    #[test]
    fn parses_and_records_canonical_params() {
        let profile = fixture();
        assert_eq!(profile.strategy.as_ref().map(|s| s.version), Some(3));
        assert_eq!(
            profile.max_risk_per_trade_bps, 1_000,
            "the 10% cap by default"
        );
        assert_eq!(profile.max_total_risk_bps, 3_000, "the 30% cap by default");
        assert_eq!(profile.rules.max_snapshot_age, Duration::minutes(1));
        assert!(profile.discovery, "discovery is on by default");
        // Formatting and the candidate sources do not change the recorded
        // version.
        let mut other = fixture_json();
        other["watchlist"] = serde_json::json!([MINT_B]);
        other["discovery"] = false.into();
        // Not recorded with a legacy version: it only cuts new entries.
        other["max_risk_per_trade_bps"] = 250.into();
        other["max_total_risk_bps"] = 250.into();
        let reparsed = PaperProfile::parse(&serde_json::to_string_pretty(&other).unwrap()).unwrap();
        assert!(!reparsed.discovery);
        assert_eq!(reparsed.max_risk_per_trade_bps, 250);
        assert_eq!(reparsed.max_total_risk_bps, 250);
        assert_eq!(reparsed.versioned_params, profile.versioned_params);
        let mut changed = fixture_json();
        changed["costs"]["dex_fee_bps"] = 30.into();
        assert_ne!(
            PaperProfile::parse(&changed.to_string())
                .unwrap()
                .versioned_params,
            profile.versioned_params
        );
    }

    #[test]
    fn the_legacy_strategy_is_optional_and_below_the_typed_versions() {
        let mut json = fixture_json();
        json.as_object_mut().unwrap().remove("strategy");
        let profile = PaperProfile::parse(&json.to_string()).unwrap();
        assert!(profile.strategy.is_none() && profile.versioned_params.is_none());
        let mut json = fixture_json();
        json["strategy"]["version"] = STRATEGY_VERSION_FLOOR.into();
        assert!(PaperProfile::parse(&json.to_string()).is_err());
    }

    #[test]
    fn load_refuses_symlinks_and_unprotected_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("profile.json");
        fs::write(&file, fixture_json().to_string()).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(PaperProfile::load(&file, false).is_ok());
        // Not root-owned, so production refuses it.
        assert!(PaperProfile::load(&file, true).is_err());
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(PaperProfile::load(&link, false).is_err());
        fs::write(&file, " ".repeat(65 * 1024)).unwrap();
        assert!(PaperProfile::load(&file, false).is_err());
    }

    #[test]
    fn refuses_unsafe_or_unknown_input() {
        let cases: [(&str, serde_json::Value); 14] = [
            ("/unknown", serde_json::json!(1)),
            ("/watchlist", serde_json::json!([])),
            (
                "/watchlist",
                serde_json::json!(["not-base58-0OIl-not-base58-0OIl-x"]),
            ),
            ("/watchlist", serde_json::json!([MINT_A, MINT_A])),
            ("/max_open_positions", serde_json::json!(11)),
            // 3 x 100 exceeds the 250 stake.
            ("/max_position_usd", serde_json::json!("100")),
            ("/goal_multiple", serde_json::json!("1")),
            // Above the code cap of 10%, or nothing.
            ("/max_risk_per_trade_bps", serde_json::json!(1_001)),
            ("/max_risk_per_trade_bps", serde_json::json!(0)),
            // Above the code cap of 30%, nothing, or below the 10% per trade.
            ("/max_total_risk_bps", serde_json::json!(3_001)),
            ("/max_total_risk_bps", serde_json::json!(0)),
            ("/max_total_risk_bps", serde_json::json!(999)),
            ("/strategy/stop_loss_bps", serde_json::json!(1)),
            ("/costs/failure_rate", serde_json::json!("1")),
        ];
        for (pointer, value) in cases {
            let mut json = fixture_json();
            match json.pointer_mut(pointer) {
                Some(slot) => *slot = value,
                // A top-level field the fixture leaves out.
                None => json[pointer.trim_start_matches('/')] = value,
            }
            assert!(PaperProfile::parse(&json.to_string()).is_err(), "{pointer}");
        }
        let mut json = fixture_json();
        json["filter"]["allowed_extensions"] = serde_json::json!(["unknown"]);
        assert!(PaperProfile::parse(&json.to_string()).is_err());
    }

    #[test]
    fn the_agent_is_off_unless_configured_and_config_only_lowers_its_caps() {
        assert!(fixture().agent.is_none());
        let mut json = fixture_json();
        json["agent"] = serde_json::json!({});
        let agent = PaperProfile::parse(&json.to_string())
            .unwrap()
            .agent
            .unwrap();
        assert_eq!((agent.max_calls_per_hour, agent.max_calls_per_day), (4, 48));
        assert!(agent.new_strategies_enabled);
        json["agent"] = serde_json::json!({"max_calls_per_hour": 2, "max_calls_per_day": 10,
            "new_strategies_enabled": false});
        let agent = PaperProfile::parse(&json.to_string())
            .unwrap()
            .agent
            .unwrap();
        assert_eq!((agent.max_calls_per_hour, agent.max_calls_per_day), (2, 10));
        assert!(!agent.new_strategies_enabled);
        for agent in [
            serde_json::json!({"max_calls_per_hour": 5}),
            serde_json::json!({"max_calls_per_day": 49}),
            serde_json::json!({"max_calls_per_hour": 0}),
            serde_json::json!({"model": "x"}),
        ] {
            json["agent"] = agent.clone();
            assert!(PaperProfile::parse(&json.to_string()).is_err(), "{agent}");
        }
    }
}
