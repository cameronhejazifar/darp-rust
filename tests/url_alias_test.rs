//! Service URL aliases (`urls`): vhost rendering, config deserialization,
//! pre_config merge behaviour, hostname validation, and deploy-plan validation.
//!
//! `cmd_deploy` itself needs a container engine and sudo, so the pure pieces it is
//! built from — the vhost renderer, the deployment planner, and the validators —
//! are tested directly. That is why they are public.

use darp::alias::{AliasErrorReason, validate_and_normalize_alias};
use darp::commands::{
    PlannedService, build_host_proxy_vhost, build_hosts_lines, build_portmap,
    build_vhost_container_conf, plan_deployment, validate_plan_aliases,
};
use darp::config::{Config, DarpPaths, merge_values};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A planned HTTP service on port 50100 with the canonical `{svc}.{domain}.test`.
fn planned(domain: &str, group: &str, service: &str, aliases: &[&str]) -> PlannedService {
    planned_full(domain, group, service, "http", 50100, aliases)
}

fn planned_full(
    domain: &str,
    group: &str,
    service: &str,
    connection_type: &str,
    proxy_port: u16,
    aliases: &[&str],
) -> PlannedService {
    PlannedService {
        domain: domain.to_string(),
        group: group.to_string(),
        service: service.to_string(),
        connection_type: connection_type.to_string(),
        proxy_port,
        debug_port: 13000,
        canonical_hostname: format!("{service}.{domain}.test"),
        aliases: aliases.iter().map(|s| s.to_string()).collect(),
    }
}

fn reason_of(input: &str) -> AliasErrorReason {
    validate_and_normalize_alias(input).expect_err(input).reason
}

/// `DarpPaths` rooted at an arbitrary directory, so a test can point deploy
/// planning at a scratch `~/.darp` without touching the engineer's real one.
fn darp_paths(root: &std::path::Path) -> DarpPaths {
    DarpPaths {
        _darp_root: root.to_path_buf(),
        config_path: root.join("config.json"),
        portmap_path: root.join("portmap.json"),
        dnsmasq_dir: root.join("dnsmasq.d"),
        vhost_container_conf: root.join("vhost_container.conf"),
        hosts_container_path: root.join("hosts_container"),
        nginx_conf_path: root.join("nginx.conf"),
        container_host_ip_path: root.join("container_host_ip"),
    }
}

/// A scratch deployment: a `~/.darp` root pre-populated with sentinel artifacts, a
/// projects directory containing `folders`, and a config whose `{loc}` token has
/// been pointed at that directory.
struct Scratch {
    _tmp: tempfile::TempDir,
    paths: DarpPaths,
    config: Config,
}

const SENTINEL: &str = "# previous deployment\n";

fn scratch(folders: &[&str], config_json: &str) -> Scratch {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("darp");
    let projects = tmp.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    for folder in folders {
        std::fs::create_dir_all(projects.join(folder)).unwrap();
    }

    let paths = darp_paths(&root);
    std::fs::write(&paths.vhost_container_conf, SENTINEL).unwrap();
    std::fs::write(&paths.hosts_container_path, SENTINEL).unwrap();
    std::fs::write(&paths.portmap_path, SENTINEL).unwrap();

    let json = config_json.replace("{loc}", &projects.to_string_lossy());
    let config: Config = serde_json::from_str(&json).expect("scratch config should deserialize");

    Scratch {
        _tmp: tmp,
        paths,
        config,
    }
}

impl Scratch {
    /// Assert every deployment artifact still holds its pre-deploy content — the
    /// atomicity guarantee a rejected plan has to keep.
    fn assert_artifacts_untouched(&self) {
        for path in [
            &self.paths.vhost_container_conf,
            &self.paths.hosts_container_path,
            &self.paths.portmap_path,
        ] {
            assert_eq!(
                std::fs::read_to_string(path).unwrap(),
                SENTINEL,
                "{} was rewritten by a failed deploy",
                path.display()
            );
        }
    }
}

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

// ---------------------------------------------------------------------------
// validate_and_normalize_alias — accepted values
// ---------------------------------------------------------------------------

#[test]
fn accepts_multi_label_hostname() {
    assert_eq!(
        validate_and_normalize_alias("local.zoo.org").unwrap(),
        "local.zoo.org"
    );
}

#[test]
fn accepts_single_label_hostname() {
    // Local DNS and hosts files can resolve a bare name, so a single label is legal
    // even though a dotted name is the sane choice.
    assert_eq!(
        validate_and_normalize_alias("intranet").unwrap(),
        "intranet"
    );
}

#[test]
fn accepts_hyphenated_and_numeric_labels() {
    assert_eq!(
        validate_and_normalize_alias("tenant-a2.portal-7.test").unwrap(),
        "tenant-a2.portal-7.test"
    );
    assert_eq!(
        validate_and_normalize_alias("8080.test").unwrap(),
        "8080.test"
    );
}

#[test]
fn accepts_punycode_hostname() {
    // An IDN reaches darp already encoded; the ASCII form is what nginx and the hosts
    // file can carry.
    assert_eq!(
        validate_and_normalize_alias("xn--e1afmkfd.xn--p1ai").unwrap(),
        "xn--e1afmkfd.xn--p1ai"
    );
}

#[test]
fn normalizes_case_to_lowercase() {
    // DNS is case-insensitive, so the artifact spelling has to be canonical or the
    // same name compares unequal to itself between config and portmap.
    assert_eq!(
        validate_and_normalize_alias("LOCAL.ZOO.ORG").unwrap(),
        "local.zoo.org"
    );
}

#[test]
fn strips_one_trailing_dns_dot() {
    assert_eq!(
        validate_and_normalize_alias("local.zoo.org.").unwrap(),
        "local.zoo.org"
    );
}

#[test]
fn trims_surrounding_whitespace() {
    assert_eq!(
        validate_and_normalize_alias("  local.zoo.org \n").unwrap(),
        "local.zoo.org"
    );
}

#[test]
fn accepts_maximum_length_label_and_hostname() {
    let label = "a".repeat(63);
    assert_eq!(validate_and_normalize_alias(&label).unwrap(), label);

    // 3 × 63 + 61 + 3 separators = 253, the RFC 1035 ceiling.
    let max = format!("{label}.{label}.{label}.{}", "b".repeat(61));
    assert_eq!(max.len(), 253);
    assert_eq!(validate_and_normalize_alias(&max).unwrap(), max);
}

// ---------------------------------------------------------------------------
// validate_and_normalize_alias — rejected values
// ---------------------------------------------------------------------------

#[test]
fn rejects_empty_and_whitespace_only_aliases() {
    assert_eq!(reason_of(""), AliasErrorReason::Empty);
    assert_eq!(reason_of("   "), AliasErrorReason::Empty);
    assert_eq!(reason_of("\t\n"), AliasErrorReason::Empty);
    // A lone dot normalizes away to nothing.
    assert_eq!(reason_of("."), AliasErrorReason::Empty);
}

#[test]
fn rejects_url_schemes() {
    assert_eq!(reason_of("http://local.zoo.org"), AliasErrorReason::Scheme);
    assert_eq!(reason_of("https://local.zoo.org"), AliasErrorReason::Scheme);
    // A colon whose tail isn't numeric is a scheme that lost its slashes, not a port.
    assert_eq!(reason_of("mailto:zoo"), AliasErrorReason::Scheme);
}

#[test]
fn rejects_port_path_query_fragment_and_userinfo() {
    assert_eq!(reason_of("local.zoo.org:8080"), AliasErrorReason::Port);
    assert_eq!(reason_of("local.zoo.org/admin"), AliasErrorReason::Path);
    assert_eq!(reason_of("local.zoo.org?a=1"), AliasErrorReason::Query);
    assert_eq!(reason_of("local.zoo.org#top"), AliasErrorReason::Fragment);
    assert_eq!(reason_of("user@local.zoo.org"), AliasErrorReason::UserInfo);
}

#[test]
fn rejects_whitespace_and_control_characters_inside_the_hostname() {
    // These are what would break a hosts-file line or an nginx directive; the label
    // charset rule catches them all.
    for bad in [
        "local zoo.org",
        "local\tzoo.org",
        "local\nzoo.org",
        "local\rzoo.org",
        "local\u{7}zoo.org",
    ] {
        assert_eq!(
            reason_of(bad),
            AliasErrorReason::InvalidCharacter,
            "expected {bad:?} to be rejected"
        );
    }
}

#[test]
fn rejects_nginx_and_hosts_file_delimiters() {
    // A stray `;` or `{` in a server_name would end the directive early and take the
    // whole shared reverse-proxy config down with it.
    for bad in [
        "local.zoo.org;",
        "local.zoo.org{",
        "local.zoo.org}",
        "local.zoo.org\"",
        "local.zoo.org'",
        "local\\zoo.org",
    ] {
        assert_eq!(
            reason_of(bad),
            AliasErrorReason::InvalidCharacter,
            "expected {bad:?} to be rejected"
        );
    }
}

#[test]
fn rejects_wildcard_hostnames() {
    // nginx would happily accept `*.zoo.org` as a server_name, but darp has no
    // wildcard hosts-file or dnsmasq story to match it.
    assert_eq!(reason_of("*.zoo.org"), AliasErrorReason::Wildcard);
    assert_eq!(reason_of("*"), AliasErrorReason::Wildcard);
}

#[test]
fn rejects_ip_address_literals() {
    // The feature names alternate hostnames, not alternate listener addresses.
    assert_eq!(reason_of("127.0.0.1"), AliasErrorReason::IpLiteral);
    assert_eq!(reason_of("192.168.1.10"), AliasErrorReason::IpLiteral);
    assert_eq!(reason_of("127.0.0.1."), AliasErrorReason::IpLiteral);
    assert_eq!(reason_of("::1"), AliasErrorReason::IpLiteral);
    assert_eq!(reason_of("2001:db8::1"), AliasErrorReason::IpLiteral);
    assert_eq!(reason_of("[::1]"), AliasErrorReason::IpLiteral);
    assert_eq!(reason_of("[::1]:8080"), AliasErrorReason::IpLiteral);
}

#[test]
fn rejects_malformed_label_structure() {
    assert_eq!(reason_of("local..zoo.org"), AliasErrorReason::EmptyLabel);
    assert_eq!(reason_of(".local.zoo.org"), AliasErrorReason::EmptyLabel);
    assert_eq!(reason_of("local.zoo.org.."), AliasErrorReason::TrailingDots);
    assert_eq!(reason_of("-zoo.org"), AliasErrorReason::LeadingHyphen);
    assert_eq!(reason_of("zoo-.org"), AliasErrorReason::TrailingHyphen);
}

#[test]
fn rejects_oversized_labels_and_hostnames() {
    let long_label = "a".repeat(64);
    assert_eq!(
        reason_of(&format!("{long_label}.org")),
        AliasErrorReason::LabelTooLong
    );

    let label = "a".repeat(63);
    let too_long = format!("{label}.{label}.{label}.{label}");
    assert!(too_long.len() > 253);
    assert_eq!(reason_of(&too_long), AliasErrorReason::HostnameTooLong);
}

#[test]
fn rejects_raw_non_ascii_hostnames() {
    // Punycode conversion is the engineer's job — darp has no IDNA encoder, and a raw
    // UTF-8 name in a hosts file resolves for nobody.
    assert_eq!(reason_of("münchen.test"), AliasErrorReason::NonAscii);
    assert_eq!(reason_of("зоо.рф"), AliasErrorReason::NonAscii);
}

#[test]
fn error_preserves_the_original_input() {
    let err = validate_and_normalize_alias("  HTTPS://Local.Zoo.Org  ").unwrap_err();
    assert_eq!(err.input, "  HTTPS://Local.Zoo.Org  ");
    assert!(err.to_string().contains("HTTPS://Local.Zoo.Org"));
    assert!(err.to_string().contains("must not include a URL scheme"));
}

// ---------------------------------------------------------------------------
// validate_plan_aliases — normalization in place
// ---------------------------------------------------------------------------

#[test]
fn plan_validation_normalizes_aliases_in_place() {
    let mut services = vec![planned(
        "comagine",
        ".",
        "portal-website",
        &["LOCAL.ZOO.ORG", " local.care.org. "],
    )];

    validate_plan_aliases(&mut services).expect("valid aliases");

    assert_eq!(services[0].aliases, vec!["local.zoo.org", "local.care.org"]);
}

#[test]
fn plan_validation_accepts_a_plan_with_no_aliases() {
    let mut services = vec![planned("comagine", ".", "nevada-hdr", &[])];
    assert!(validate_plan_aliases(&mut services).is_ok());
}

// ---------------------------------------------------------------------------
// validate_plan_aliases — consolidated reporting
// ---------------------------------------------------------------------------

#[test]
fn plan_validation_reports_every_invalid_alias_together() {
    let mut services = vec![
        planned(
            "comagine",
            ".",
            "portal-website",
            &["https://local.zoo.org", "local.zoo.org:8080"],
        ),
        planned("comagine", "admin", "dashboard", &["local zoo.org"]),
    ];

    let err = validate_plan_aliases(&mut services)
        .unwrap_err()
        .to_string();

    assert!(err.contains("Cannot deploy because some URL aliases are not valid hostnames:"));
    assert!(err.contains("comagine/./portal-website"));
    assert!(err.contains(r#""https://local.zoo.org" — aliases must not include a URL scheme"#));
    assert!(err.contains(r#""local.zoo.org:8080" — aliases must not include a port"#));
    assert!(err.contains("comagine/admin/dashboard"));
    assert!(err.contains(
        r#""local zoo.org" — hostname labels may contain only letters, digits, and hyphens"#
    ));
    assert!(err.contains("No deployment artifacts were changed."));
}

#[test]
fn plan_validation_error_ordering_is_deterministic() {
    // Discovery order comes from read_dir and is unstable, so the report sorts by
    // service identity and then by the alias text as configured.
    let mut services = vec![
        planned("comagine", "sites", "zoo", &["b.zoo.org:1", "a.zoo.org:1"]),
        planned("comagine", ".", "portal-website", &["c.care.org:1"]),
    ];

    let err = validate_plan_aliases(&mut services)
        .unwrap_err()
        .to_string();

    let portal = err.find("comagine/./portal-website").unwrap();
    let zoo = err.find("comagine/sites/zoo").unwrap();
    let a = err.find("a.zoo.org:1").unwrap();
    let b = err.find("b.zoo.org:1").unwrap();
    assert!(portal < zoo, "services must be sorted by identity");
    assert!(a < b, "aliases must be sorted within a service");
}

#[test]
fn plan_validation_rejects_invalid_tcp_service_aliases() {
    // A TCP service gets no vhost, but it does get hosts entries — a malformed value
    // is just as damaging there.
    let mut services = vec![planned_full(
        "comagine",
        ".",
        "queue",
        "tcp",
        50100,
        &["local.queue.org:5672"],
    )];

    let err = validate_plan_aliases(&mut services)
        .unwrap_err()
        .to_string();
    assert!(err.contains("comagine/./queue"));
    assert!(err.contains("aliases must not include a port"));
}

#[test]
fn plan_validation_rejects_an_empty_alias_rather_than_dropping_it() {
    // Silently discarding `""` hid a config typo; the engineer should hear about it.
    let mut services = vec![planned("comagine", ".", "portal-website", &["", "  "])];
    assert!(validate_plan_aliases(&mut services).is_err());
}

// ---------------------------------------------------------------------------
// Deployment planning: discovery is read-only
// ---------------------------------------------------------------------------

const SCRATCH_CONFIG: &str = r#"{
    "domains": {
        "comagine": {
            "location": "{loc}",
            "groups": {
                ".": {
                    "services": {
                        "portal-website": { "urls": ["LOCAL.ZOO.ORG"] },
                        "queue": { "connection_type": "tcp" }
                    }
                }
            }
        }
    }
}"#;

#[test]
fn plan_deployment_discovers_folders_with_their_config() {
    let s = scratch(&["portal-website", "queue"], SCRATCH_CONFIG);

    let mut services = plan_deployment(&s.paths, &s.config).unwrap();
    services.sort_by(|a, b| a.service.cmp(&b.service));

    assert_eq!(services.len(), 2);
    assert_eq!(services[0].service, "portal-website");
    assert_eq!(
        services[0].canonical_hostname,
        "portal-website.comagine.test"
    );
    assert_eq!(services[0].aliases, vec!["LOCAL.ZOO.ORG"]);
    assert!(services[0].is_host_routed());
    assert_eq!(services[1].service, "queue");
    assert_eq!(services[1].connection_type, "tcp");
    assert!(!services[1].is_host_routed());

    // Planning is discovery only.
    s.assert_artifacts_untouched();
}

#[test]
fn plan_deployment_ignores_services_configured_but_absent_on_disk() {
    // Only `queue` exists as a folder; `portal-website` is configured but not cloned.
    let s = scratch(&["queue"], SCRATCH_CONFIG);

    let services = plan_deployment(&s.paths, &s.config).unwrap();

    assert_eq!(services.len(), 1);
    assert_eq!(services[0].service, "queue");
}

#[test]
fn invalid_alias_leaves_existing_deployment_artifacts_untouched() {
    let s = scratch(
        &["portal-website"],
        r#"{
            "domains": {
                "comagine": {
                    "location": "{loc}",
                    "groups": {
                        ".": {
                            "services": {
                                "portal-website": { "urls": ["https://local.zoo.org"] }
                            }
                        }
                    }
                }
            }
        }"#,
    );

    let mut services = plan_deployment(&s.paths, &s.config).unwrap();
    assert!(validate_plan_aliases(&mut services).is_err());

    // The nginx vhost file, the container hosts file, and the portmap all predate this
    // deploy and must survive it — nothing was truncated on the way to the error.
    s.assert_artifacts_untouched();
}

// ---------------------------------------------------------------------------
// Artifact generation from a validated plan
// ---------------------------------------------------------------------------

#[test]
fn vhost_conf_renders_canonical_plus_aliases_per_service() {
    let services = vec![
        planned_full(
            "comagine",
            ".",
            "portal-website",
            "http",
            50100,
            &["local.zoo.org"],
        ),
        planned_full("comagine", ".", "chat", "websocket", 50101, &["ws.zoo.org"]),
    ];

    let conf = build_vhost_container_conf(&services, "host.docker.internal");

    assert!(conf.contains("server_name portal-website.comagine.test;"));
    assert!(conf.contains("server_name local.zoo.org;"));
    assert!(conf.contains("server_name chat.comagine.test;"));
    assert!(conf.contains("server_name ws.zoo.org;"));
    assert_eq!(conf.matches("server {").count(), 4);
    assert!(conf.contains("proxy_pass http://host.docker.internal:50100/;"));
    assert!(conf.contains("proxy_pass http://host.docker.internal:50101/;"));
}

#[test]
fn vhost_conf_omits_tcp_services() {
    // nginx cannot route plain TCP by hostname; a TCP service is reached on its port.
    let services = vec![planned_full(
        "comagine",
        ".",
        "queue",
        "tcp",
        50100,
        &["local.queue.org"],
    )];

    let conf = build_vhost_container_conf(&services, "gw");

    assert!(conf.is_empty());
}

#[test]
fn hosts_lines_cover_canonical_and_alias_for_every_connection_type() {
    let services = vec![
        planned_full(
            "comagine",
            ".",
            "portal-website",
            "http",
            50100,
            &["local.zoo.org"],
        ),
        planned_full("comagine", ".", "queue", "tcp", 50101, &["local.queue.org"]),
    ];

    let lines = build_hosts_lines(&services);

    assert_eq!(
        lines,
        vec![
            "0.0.0.0   portal-website.comagine.test\n",
            "0.0.0.0   local.zoo.org\n",
            "0.0.0.0   queue.comagine.test\n",
            "0.0.0.0   local.queue.org\n",
        ]
    );
}

#[test]
fn portmap_records_normalized_aliases_alongside_port_and_type() {
    let services = vec![planned_full(
        "comagine",
        ".",
        "portal-website",
        "http",
        50100,
        &["local.zoo.org"],
    )];

    let portmap = build_portmap(&services, &["comagine".to_string()]);
    let entry = &portmap["comagine"]["."]["portal-website"];

    assert_eq!(entry["port"], 50100);
    assert_eq!(entry["type"], "http");
    assert_eq!(entry["debug_port"], 13000);
    assert_eq!(entry["urls"], serde_json::json!(["local.zoo.org"]));
}

#[test]
fn portmap_omits_urls_for_services_without_aliases() {
    let services = vec![planned("comagine", ".", "nevada-hdr", &[])];

    let portmap = build_portmap(&services, &[]);

    assert!(portmap["comagine"]["."]["nevada-hdr"].get("urls").is_none());
}

#[test]
fn portmap_keeps_a_configured_domain_with_no_service_folders() {
    // `darp urls` prints a header per portmap domain; an empty domain still shows.
    let portmap = build_portmap(&[], &["comagine".to_string()]);

    assert_eq!(portmap["comagine"], serde_json::json!({}));
}
