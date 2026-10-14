use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use fm_browser_input::{
    BrowserError, BrowserLimits, CssError, CustomCss, HostPattern, HostRules,
    NavigationBlockReason, NavigationGrant, NavigationPolicy, NetworkLimits, NetworkPolicy,
    NetworkPolicyError, NormalizedUrl, SurfaceConfig, UrlHost, VideoDimensions,
};

const MAX: usize = NetworkPolicy::DEFAULT_MAX_URL_BYTES;

fn normalized(url: &str) -> String {
    NormalizedUrl::parse(url, MAX).unwrap().as_str().to_owned()
}

fn malformed(url: &str) -> &'static str {
    match NormalizedUrl::parse(url, MAX) {
        Err(NetworkPolicyError::MalformedUrl { reason }) => reason,
        other => panic!("{url} should be malformed, got {other:?}"),
    }
}

fn limits(max_redirects: usize) -> NetworkLimits {
    NetworkLimits {
        max_redirects,
        ..NetworkLimits::default()
    }
}

fn private_denial(error: &NetworkPolicyError) -> bool {
    matches!(
        error,
        NetworkPolicyError::Navigation(blocked)
            if matches!(blocked.reason, NavigationBlockReason::PrivateNetwork { .. })
    )
}

#[test]
fn urls_are_normalized_deterministically() {
    assert_eq!(
        normalized("HTTPS://Example.COM.:443/a/./b/../c?x=1#Frag"),
        "https://example.com/a/c?x=1#Frag"
    );
    assert_eq!(normalized("http://example.com"), "http://example.com/");
    assert_eq!(normalized("http://example.com:80/"), "http://example.com/");
    assert_eq!(
        normalized("https://example.com:8443/x/"),
        "https://example.com:8443/x/"
    );
    assert_eq!(normalized("http://example.com:/"), "http://example.com/");
    assert_eq!(
        normalized("https://example.com/a/%2E%2e/b/%2e"),
        "https://example.com/b/"
    );
    assert_eq!(
        normalized("https://example.com/../../etc"),
        "https://example.com/etc"
    );
    assert_eq!(normalized("http://0x7f000001/"), "http://127.0.0.1/");
    assert_eq!(normalized("http://0177.1/"), "http://127.0.0.1/");
    assert_eq!(normalized("http://[0:0::1]:8080/"), "http://[::1]:8080/");
    assert_eq!(normalized("file:///tmp/a/../b.html"), "file:///tmp/b.html");
    assert_eq!(normalized("about:blank"), "about:blank");

    let url = NormalizedUrl::parse("https://Sub.Example.com:8443/p?q#f", MAX).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(
        url.host(),
        Some(&UrlHost::Domain("sub.example.com".to_owned()))
    );
    assert_eq!(url.port(), Some(8443));
    assert_eq!(url.path(), "/p");
    assert_eq!(url.query(), Some("q"));
    assert_eq!(url.fragment(), Some("f"));
    assert_eq!(url.network_key(), "https://sub.example.com:8443/p?q");
    assert_eq!(
        NormalizedUrl::parse("https://example.com/", MAX)
            .unwrap()
            .effective_port(),
        Some(443)
    );
    assert_eq!(
        NormalizedUrl::parse("http://[::1]/", MAX).unwrap().host(),
        Some(&UrlHost::Ipv6(Ipv6Addr::LOCALHOST))
    );
}

#[test]
fn ambiguous_or_unbounded_urls_are_rejected_explicitly() {
    assert_eq!(
        NormalizedUrl::parse("https://user:pass@example.com/", MAX),
        Err(NetworkPolicyError::CredentialsInUrl)
    );
    assert_eq!(
        NormalizedUrl::parse("https://evil.example@good.example/", MAX),
        Err(NetworkPolicyError::CredentialsInUrl)
    );
    assert_eq!(
        malformed("https://evil.example\\@good.example/"),
        "backslash"
    );
    assert_eq!(
        malformed("https://exa mple.com/"),
        "whitespace or control character"
    );
    assert_eq!(
        malformed("https://example.com/\n"),
        "whitespace or control character"
    );
    assert_eq!(
        malformed("https://b\u{fc}cher.example/"),
        "non-ASCII character"
    );
    assert_eq!(
        malformed("https://example.com:70000/"),
        "port exceeds 65535"
    );
    assert_eq!(malformed("https://example.com:8x/"), "invalid port");
    assert_eq!(
        malformed("https:example.com"),
        "scheme requires an authority"
    );
    assert_eq!(malformed("https:///path"), "empty host");
    assert_eq!(malformed("//example.com/"), "missing or invalid scheme");
    assert_eq!(malformed("https://exa*mple.com/"), "invalid host character");
    assert_eq!(
        malformed("https://exa%2emple.com/"),
        "invalid host character"
    );
    assert_eq!(malformed("http://1.2.3.256/"), "invalid IPv4 address");
    assert_eq!(malformed("http://[::g]/"), "invalid IPv6 literal");
    assert_eq!(
        malformed("http://[::1]x/"),
        "unexpected text after IPv6 literal"
    );
    assert_eq!(malformed("http://a..b.example/"), "empty host label");
    assert_eq!(
        malformed("http://-bad.example/"),
        "host label starts or ends with a hyphen"
    );
    let long_label = format!("https://{}.example/", "a".repeat(64));
    assert_eq!(malformed(&long_label), "host label exceeds 63 bytes");
    assert_eq!(malformed("file://host:1/x"), "file URL with a port");

    let long = format!("https://example.com/{}", "a".repeat(100));
    assert_eq!(
        NormalizedUrl::parse(&long, 64),
        Err(NetworkPolicyError::UrlTooLong {
            actual: long.len(),
            maximum: 64,
        })
    );
}

#[test]
fn host_patterns_match_exact_hosts_and_strict_subdomains() {
    let wildcard = HostPattern::parse("*.Example.com.").unwrap();
    assert_eq!(wildcard.to_string(), "*.example.com");
    let host = |value: &str| UrlHost::parse(value).unwrap();
    assert!(wildcard.matches(&host("a.example.com")));
    assert!(wildcard.matches(&host("a.b.example.com")));
    assert!(!wildcard.matches(&host("example.com")));
    assert!(!wildcard.matches(&host("badexample.com")));
    assert!(!wildcard.matches(&host("example.com.evil.net")));

    let exact = HostPattern::parse("Example.com").unwrap();
    assert!(exact.matches(&host("example.com")));
    assert!(!exact.matches(&host("www.example.com")));
    assert!(
        HostPattern::parse("0x08080808")
            .unwrap()
            .matches(&host("8.8.8.8"))
    );
    assert!(
        HostPattern::parse("[2001:db8::1]")
            .unwrap()
            .matches(&host("[2001:db8:0::1]"))
    );

    for (pattern, reason) in [
        ("*.com", "wildcard suffix must have at least two labels"),
        (
            "a.*.example.com",
            "wildcard is only allowed as a leading `*.` label",
        ),
        (
            "*.*.example.com",
            "wildcard is only allowed as the leftmost label",
        ),
        ("*", "wildcard is only allowed as a leading `*.` label"),
        ("*.10.0.0.1", "wildcards cannot apply to IP literals"),
        ("exa mple.com", "not a valid host"),
    ] {
        assert_eq!(
            HostPattern::parse(pattern),
            Err(NetworkPolicyError::InvalidHostPattern {
                pattern: pattern.to_owned(),
                reason,
            }),
            "{pattern}"
        );
    }
}

#[test]
fn host_rules_are_bounded_and_deny_wins() {
    let mut rules = HostRules::new(3);
    assert!(rules.allow("*.example.com").unwrap());
    assert!(!rules.allow("*.EXAMPLE.com").unwrap());
    assert!(rules.deny("ads.example.com").unwrap());
    assert!(rules.allow("cdn.example.net").unwrap());
    assert_eq!(
        rules.deny("other.example"),
        Err(NetworkPolicyError::HostRuleLimit { maximum: 3 })
    );
    assert_eq!(rules.allowed().len() + rules.denied().len(), 3);

    let host = |value: &str| UrlHost::parse(value).unwrap();
    rules.evaluate(&host("live.example.com")).unwrap();
    rules.evaluate(&host("cdn.example.net")).unwrap();
    assert_eq!(
        rules.evaluate(&host("ads.example.com")),
        Err(NetworkPolicyError::HostDenied {
            host: "ads.example.com".to_owned(),
            pattern: "ads.example.com".to_owned(),
        })
    );
    assert_eq!(
        rules.evaluate(&host("example.com")),
        Err(NetworkPolicyError::HostNotAllowed {
            host: "example.com".to_owned(),
        })
    );
    assert!(rules.remove(&HostPattern::parse("ads.example.com").unwrap()));
    rules.evaluate(&host("ads.example.com")).unwrap();

    HostRules::default()
        .evaluate(&host("anything.example"))
        .unwrap();
}

#[test]
fn network_policy_composes_normalization_navigation_and_host_rules() {
    let mut policy = NetworkPolicy::default();
    policy.hosts_mut().allow("*.example.com").unwrap();
    policy.hosts_mut().allow("10.0.0.5").unwrap();

    assert_eq!(
        policy
            .check("HTTPS://Live.Example.com/show/../index.html")
            .unwrap()
            .as_str(),
        "https://live.example.com/index.html"
    );
    assert!(matches!(
        policy.check("https://example.org/"),
        Err(NetworkPolicyError::HostNotAllowed { .. })
    ));
    // An allow rule never bypasses the private-network default.
    assert!(private_denial(
        &policy.check("http://10.0.0.5/").unwrap_err()
    ));
    assert!(private_denial(
        &NetworkPolicy::default()
            .check("http://0177.0.0.1/")
            .unwrap_err()
    ));
    assert!(matches!(
        policy.check("ftp://cdn.example.com/"),
        Err(NetworkPolicyError::Navigation(blocked))
            if matches!(blocked.reason, NavigationBlockReason::UnsupportedScheme { .. })
    ));
    assert!(private_denial(
        &policy
            .check_resolved(
                "https://live.example.com/",
                &[IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20))],
            )
            .unwrap_err()
    ));
    policy
        .check_resolved(
            "https://live.example.com/",
            &[IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))],
        )
        .unwrap();

    policy
        .navigation_mut()
        .grant(NavigationGrant::PrivateNetwork);
    policy.check("http://10.0.0.5/").unwrap();

    let tight =
        NetworkPolicy::new(NavigationPolicy::new(), HostRules::default()).with_max_url_bytes(24);
    assert!(matches!(
        tight.check("https://example.com/long/path"),
        Err(NetworkPolicyError::UrlTooLong { maximum: 24, .. })
    ));

    let error: BrowserError = NetworkPolicyError::CredentialsInUrl.into();
    assert_eq!(error.to_string(), "URL credentials are not permitted");
}

#[test]
fn redirect_chains_resolve_relative_locations_and_are_bounded() {
    let policy = NetworkPolicy::default();
    let mut chain = policy
        .begin_navigation("https://example.com/a/b/page.html?x#top", limits(6))
        .unwrap();
    assert_eq!(chain.redirects(), 0);
    let resolved = [
        ("next.html", "https://example.com/a/b/next.html"),
        ("../up.html", "https://example.com/a/up.html"),
        ("/root?q=1", "https://example.com/root?q=1"),
        ("?q=2", "https://example.com/root?q=2"),
        ("//cdn.example.com/x", "https://cdn.example.com/x"),
        ("HTTPS://Other.Example:443/y", "https://other.example/y"),
    ];
    for (location, expected) in resolved {
        assert_eq!(chain.follow(&policy, location).unwrap().as_str(), expected);
    }
    assert_eq!(chain.redirects(), 6);
    assert_eq!(
        chain.initial().as_str(),
        "https://example.com/a/b/page.html?x#top"
    );
    assert_eq!(chain.hops().count(), 7);
    assert_eq!(
        chain.follow(&policy, "/more"),
        Err(NetworkPolicyError::TooManyRedirects { maximum: 6 })
    );
    assert_eq!(chain.redirects(), 6);

    let mut zero = policy
        .begin_navigation("https://example.com/", limits(0))
        .unwrap();
    assert_eq!(
        zero.follow(&policy, "/a"),
        Err(NetworkPolicyError::TooManyRedirects { maximum: 0 })
    );
}

#[test]
fn redirects_reject_loops_downgrades_files_and_denied_hosts_atomically() {
    let mut policy = NetworkPolicy::default();
    policy.hosts_mut().deny("*.tracker.example").unwrap();
    policy.navigation_mut().grant(NavigationGrant::File);

    let mut chain = policy
        .begin_navigation("https://example.com/start", limits(8))
        .unwrap();
    chain.follow(&policy, "/second").unwrap();
    assert_eq!(
        chain.follow(&policy, "/start#elsewhere"),
        Err(NetworkPolicyError::RedirectLoop {
            url: "https://example.com/start".to_owned(),
        })
    );
    assert!(matches!(
        chain.follow(&policy, "http://example.com/plain"),
        Err(NetworkPolicyError::InsecureRedirect { .. })
    ));
    assert_eq!(
        chain.follow(&policy, "file:///etc/passwd"),
        Err(NetworkPolicyError::FileRedirect {
            to: "file:///etc/passwd".to_owned(),
        })
    );
    assert!(matches!(
        chain.follow(&policy, "https://pixel.tracker.example/"),
        Err(NetworkPolicyError::HostDenied { .. })
    ));
    assert!(private_denial(
        &chain
            .follow(&policy, "https://169.254.169.254/")
            .unwrap_err()
    ));
    assert!(matches!(
        chain.follow(&policy, ""),
        Err(NetworkPolicyError::MalformedUrl { .. })
    ));
    // Every rejected redirect left the chain untouched.
    assert_eq!(chain.redirects(), 1);
    assert_eq!(chain.current().as_str(), "https://example.com/second");

    policy.set_allow_insecure_redirects(true);
    assert_eq!(
        chain
            .follow(&policy, "http://example.com/plain")
            .unwrap()
            .as_str(),
        "http://example.com/plain"
    );
}

#[test]
fn custom_css_is_structurally_validated_and_references_are_extracted() {
    let limits = BrowserLimits::default();
    let css = CustomCss::parse(
        "/* overlay */ body { background: url(\"data:image/png;base64,AAAA\") }\n\
         .logo { background-image: URL( https://cdn.example.com/logo.png ); }\n\
         .mask { mask: url(#m); content: \"\\201C</sty\" \"le\"; }",
        limits,
    )
    .unwrap();
    assert_eq!(
        css.external_urls(),
        ["https://cdn.example.com/logo.png".to_owned()]
    );
    assert_eq!(css.inline_references(), 2);

    let mut policy = NetworkPolicy::default();
    assert_eq!(
        policy.check_css(&css).unwrap()[0].as_str(),
        "https://cdn.example.com/logo.png"
    );
    policy.hosts_mut().allow("live.example.com").unwrap();
    assert!(matches!(
        policy.check_css(&css),
        Err(NetworkPolicyError::HostNotAllowed { .. })
    ));
    let private = CustomCss::parse("a{background:url(http://192.168.0.1/x.png)}", limits).unwrap();
    assert!(private_denial(
        &NetworkPolicy::default().check_css(&private).unwrap_err()
    ));

    let cases: [(&str, CssError); 11] = [
        ("a{}</STYLE><script>", CssError::StyleBreakout { offset: 3 }),
        ("/* </style */", CssError::StyleBreakout { offset: 3 }),
        ("a{}\u{0}", CssError::ControlCharacter { offset: 3 }),
        (
            "@IMPORT 'https://x.example/a.css';",
            CssError::Import { offset: 0 },
        ),
        (
            "a{b:u\\72l(https://x.example/)}",
            CssError::EscapeOutsideString { offset: 5 },
        ),
        ("a{} /* open", CssError::UnterminatedComment { offset: 4 }),
        (
            "a{content:\"open}",
            CssError::UnterminatedString { offset: 10 },
        ),
        (
            "a{b:url(img/x.png)}",
            CssError::RelativeUrlReference { offset: 4 },
        ),
        ("a{b:url(x y)}", CssError::InvalidUrlReference { offset: 4 }),
        (
            "a{b:url('h\\ttps://x')}",
            CssError::InvalidUrlReference { offset: 4 },
        ),
        (
            "a{b:image-set('https://x.example/a.png' 1x)}",
            CssError::UnsupportedFunction {
                offset: 4,
                name: "image-set".to_owned(),
            },
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(CustomCss::parse(input, limits), Err(expected), "{input}");
    }

    let many = (0..=CustomCss::MAX_EXTERNAL_REFERENCES)
        .map(|index| format!(".a{index}{{b:url(https://x.example/{index}.png)}}"))
        .collect::<Vec<_>>()
        .concat();
    assert_eq!(
        CustomCss::parse(many, limits),
        Err(CssError::TooManyReferences {
            maximum: CustomCss::MAX_EXTERNAL_REFERENCES,
        })
    );
    assert_eq!(
        CustomCss::parse(
            "abcd",
            BrowserLimits {
                max_css_bytes: 3,
                ..limits
            }
        ),
        Err(CssError::TooLarge {
            actual: 4,
            maximum: 3,
        })
    );
}

#[test]
fn surface_custom_css_rejects_style_breakout_without_changing_config() {
    let mut config = SurfaceConfig::new(VideoDimensions::new(2, 2).unwrap());
    let limits = BrowserLimits::default();
    config
        .set_custom_css(Some("body { color: red; }".to_owned()), limits)
        .unwrap();
    assert_eq!(
        config.set_custom_css(
            Some("body{}</style><script>alert(1)</script>".to_owned()),
            limits,
        ),
        Err(BrowserError::Css(CssError::StyleBreakout { offset: 6 }))
    );
    assert_eq!(config.custom_css(), Some("body { color: red; }"));
    config.set_custom_css(None, limits).unwrap();
    assert_eq!(config.custom_css(), None);
}

#[test]
fn network_policy_blocks_ipv6_embedded_private_ipv4() {
    let policy = NetworkPolicy::default();
    for url in [
        "https://[64:ff9b::a00:1]/",
        "https://[2002:c0a8:101::]/",
        "https://[fec0::1]/",
    ] {
        assert!(private_denial(&policy.check(url).unwrap_err()), "{url}");
    }
    let nat64: IpAddr = "64:ff9b::7f00:1".parse().unwrap();
    assert!(private_denial(
        &policy
            .check_resolved("https://public.example/", &[nat64])
            .unwrap_err()
    ));
}
