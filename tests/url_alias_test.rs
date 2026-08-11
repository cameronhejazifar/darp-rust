//! Service URL aliases (`urls`): vhost rendering, config deserialization, and
//! pre_config merge behaviour.
//!
//! `cmd_deploy` itself needs a container engine and sudo, so the vhost renderer is
//! tested directly — that is why `build_host_proxy_vhost` is public.

use darp::commands::build_host_proxy_vhost;
use darp::config::{Config, merge_values};

#[test]
fn vhost_substitutes_url_gateway_and_port() {
    let vhost = build_host_proxy_vhost("local.zoo.org", "host.docker.internal", 50123);

    assert!(vhost.contains("server_name local.zoo.org;"));
    assert!(vhost.contains("proxy_pass http://host.docker.internal:50123/;"));

    // No placeholder survives substitution — a leftover token would make nginx
    // refuse to start, taking down every service's vhost with it.
    assert!(!vhost.contains("{url}"));
    assert!(!vhost.contains("{host_gateway}"));
    assert!(!vhost.contains("{port}"));
}

#[test]
fn vhost_forwards_websocket_upgrade() {
    // Vite HMR arrives on the alias hostname over the same port 80 the page used,
    // so every alias vhost has to forward the upgrade handshake.
    let vhost = build_host_proxy_vhost("local.zoo.org", "host.docker.internal", 50123);

    assert!(vhost.contains("proxy_http_version 1.1;"));
    assert!(vhost.contains("proxy_set_header Upgrade $http_upgrade;"));
    assert!(vhost.contains("proxy_set_header Connection $connection_upgrade;"));
}

#[test]
fn vhost_passes_original_host_through() {
    // A multi-tenant app branches on the hostname it was reached by, so the alias
    // name — not the canonical one — has to survive the proxy hop.
    let vhost = build_host_proxy_vhost("local.aianqio.org", "gw", 50100);

    assert!(vhost.contains("proxy_set_header Host $host;"));
}

#[test]
fn canonical_and_alias_share_one_upstream_port() {
    let canonical = build_host_proxy_vhost("portal-website.comagine.test", "gw", 50100);
    let alias = build_host_proxy_vhost("local.zoo.org", "gw", 50100);

    assert!(canonical.contains("server_name portal-website.comagine.test;"));
    assert!(alias.contains("server_name local.zoo.org;"));
    assert!(canonical.contains("proxy_pass http://gw:50100/;"));
    assert!(alias.contains("proxy_pass http://gw:50100/;"));
}

#[test]
fn service_urls_deserialize() {
    let json = r#"{
        "domains": {
            "comagine": {
                "location": "{home}/comagine",
                "groups": {
                    ".": {
                        "services": {
                            "portal-website": {
                                "urls": ["local.zoo.org", "local.care.org"]
                            }
                        }
                    }
                }
            }
        }
    }"#;

    let cfg: Config = serde_json::from_str(json).expect("config with urls should deserialize");

    let urls = cfg.domains.as_ref().unwrap()["comagine"]
        .groups
        .as_ref()
        .unwrap()["."]
        .services
        .as_ref()
        .unwrap()["portal-website"]
        .urls
        .as_ref()
        .expect("urls should be populated");

    assert_eq!(
        urls,
        &vec!["local.zoo.org".to_string(), "local.care.org".to_string()]
    );
}

#[test]
fn service_urls_round_trip_through_serialization() {
    // `darp config show` and `darp config set` both re-serialize the config, so the
    // field has to survive a round trip rather than being silently dropped.
    let json = r#"{"domains":{"comagine":{"location":"{home}/comagine","groups":{".":{"services":{"portal-website":{"urls":["local.zoo.org"]}}}}}}}"#;

    let cfg: Config = serde_json::from_str(json).unwrap();
    let back = serde_json::to_string(&cfg).unwrap();

    assert!(back.contains(r#""urls":["local.zoo.org"]"#));
}

#[test]
fn absent_urls_is_not_serialized() {
    // skip_serializing_if keeps team configs diff-clean for the services that have
    // no aliases.
    let json = r#"{"domains":{"comagine":{"location":"{home}/comagine","groups":{".":{"services":{"nevada-hdr":{}}}}}}}"#;

    let cfg: Config = serde_json::from_str(json).unwrap();
    let back = serde_json::to_string(&cfg).unwrap();

    assert!(!back.contains("urls"));
}

#[test]
fn pre_config_merge_appends_alias_lists() {
    // The team config supplies the shared aliases; an engineer's personal config can
    // add one of their own. merge_values concatenates arrays.
    let team = serde_json::json!({ "urls": ["local.zoo.org"] });
    let personal = serde_json::json!({ "urls": ["local.mine.org"] });

    let merged = merge_values(team, personal);

    assert_eq!(
        merged["urls"],
        serde_json::json!(["local.zoo.org", "local.mine.org"])
    );
}

#[test]
fn star_urls_override_replaces_alias_list() {
    // There is no `*urls` field on Service, but merge_values is generic over JSON, so
    // the `*key` force-replace convention still works for a personal config that
    // wants to drop the team's aliases entirely.
    let team = serde_json::json!({ "urls": ["local.zoo.org", "local.care.org"] });
    let personal = serde_json::json!({ "*urls": ["local.mine.org"] });

    let merged = merge_values(team, personal);

    assert_eq!(merged["urls"], serde_json::json!(["local.mine.org"]));
}
