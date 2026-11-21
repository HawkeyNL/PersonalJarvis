use super::*;

#[test]
fn check_url_table() {
    let deny = ["jarvis.example.net".to_string()];
    for (url, expected) in [
        ("https://example.com/", Ok("https://example.com/")),
        (
            "https://Example.COM:443/a?b=c#frag",
            Ok("https://example.com/a?b=c"),
        ),
        (
            "https://www.nu.nl/economie/6312345/x.html",
            Ok("https://www.nu.nl/economie/6312345/x.html"),
        ),
        ("https://bücher.de/", Ok("https://xn--bcher-kva.de/")),
        ("https://example.com./", Ok("https://example.com./")),
        (
            "https://example.com/how-to-cancel-netflix",
            Ok("https://example.com/how-to-cancel-netflix"),
        ),
        (
            "https://example.com/?q=delete",
            Ok("https://example.com/?q=delete"),
        ),
        // Scheme, port, user info.
        ("http://example.com/", Err(Refusal::UrlRefused)),
        ("ftp://example.com/", Err(Refusal::UrlRefused)),
        ("file:///etc/passwd", Err(Refusal::UrlRefused)),
        ("https://example.com:8443/", Err(Refusal::UrlRefused)),
        ("https://example.com:80/", Err(Refusal::UrlRefused)),
        ("https://user@example.com/", Err(Refusal::UrlRefused)),
        ("https://user:pw@example.com/", Err(Refusal::UrlRefused)),
        // IP literals in any spelling.
        ("https://127.0.0.1/", Err(Refusal::UrlRefused)),
        ("https://8.8.8.8/", Err(Refusal::UrlRefused)),
        ("https://2130706433/", Err(Refusal::UrlRefused)),
        ("https://0x7f.1/", Err(Refusal::UrlRefused)),
        ("https://0177.0.0.1/", Err(Refusal::UrlRefused)),
        ("https://127.1/", Err(Refusal::UrlRefused)),
        ("https://[::1]/", Err(Refusal::UrlRefused)),
        ("https://[::ffff:127.0.0.1]/", Err(Refusal::UrlRefused)),
        ("https://[2606:4700::1]/", Err(Refusal::UrlRefused)),
        // Single-label and special-use names.
        ("https://intranet/", Err(Refusal::UrlRefused)),
        ("https://localhost/", Err(Refusal::UrlRefused)),
        ("https://localhost./", Err(Refusal::UrlRefused)),
        ("https://foo.localhost/", Err(Refusal::UrlRefused)),
        ("https://printer.local/", Err(Refusal::UrlRefused)),
        ("https://db.internal/", Err(Refusal::UrlRefused)),
        ("https://router.home.arpa/", Err(Refusal::UrlRefused)),
        ("https://nas.lan/", Err(Refusal::UrlRefused)),
        ("https://x.onion/", Err(Refusal::UrlRefused)),
        ("https://1.2.3.4.nip.io.test/", Err(Refusal::UrlRefused)),
        // The deny list and everything below it.
        ("https://jarvis.example.net/", Err(Refusal::UrlRefused)),
        ("https://JARVIS.example.net./", Err(Refusal::UrlRefused)),
        ("https://api.jarvis.example.net/", Err(Refusal::UrlRefused)),
        (
            "https://notjarvis.example.net/",
            Ok("https://notjarvis.example.net/"),
        ),
        // Action-like paths and queries.
        ("https://example.com/logout", Err(Refusal::ActionPath)),
        (
            "https://example.com/account/Log-Out",
            Err(Refusal::ActionPath),
        ),
        ("https://example.com/sign_out.php", Err(Refusal::ActionPath)),
        (
            "https://example.com/list/unsubscribe/",
            Err(Refusal::ActionPath),
        ),
        (
            "https://example.com/item/7/delete",
            Err(Refusal::ActionPath),
        ),
        ("https://example.com/%64elete", Err(Refusal::ActionPath)),
        (
            "https://example.com/x?action=delete",
            Err(Refusal::ActionPath),
        ),
        ("https://example.com/x?Do=1", Err(Refusal::ActionPath)),
        // Not a URL, or too long.
        ("example.com", Err(Refusal::UrlRefused)),
        ("", Err(Refusal::UrlRefused)),
    ] {
        let got = check_url(url, &deny).map(|url| url.to_string());
        assert_eq!(got, expected.map(str::to_string), "{url}");
    }
    let long = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
    assert_eq!(check_url(&long, &[]), Err(Refusal::UrlRefused));
    // An empty deny entry denies nothing.
    assert!(check_url("https://example.com/", &[String::new()]).is_ok());
}

#[test]
fn is_public_table() {
    for (ip, public) in [
        ("8.8.8.8", true),
        ("1.1.1.1", true),
        ("93.184.215.14", true),
        ("100.63.255.255", true),
        ("100.128.0.0", true),
        ("172.32.0.1", true),
        ("223.255.255.255", true),
        ("0.0.0.0", false),
        ("0.1.2.3", false),
        ("10.0.0.1", false),
        ("100.64.0.1", false),
        ("100.127.255.255", false),
        ("127.0.0.1", false),
        ("127.255.255.254", false),
        ("169.254.169.254", false),
        ("172.16.0.1", false),
        ("172.31.255.255", false),
        ("192.0.0.170", false),
        ("192.0.2.1", false),
        ("192.88.99.1", false),
        ("192.168.1.1", false),
        ("198.18.0.1", false),
        ("198.19.255.255", false),
        ("198.51.100.1", false),
        ("203.0.113.1", false),
        ("224.0.0.1", false),
        ("239.255.255.250", false),
        ("240.0.0.1", false),
        ("255.255.255.255", false),
        ("2606:4700:4700::1111", true),
        ("2a00:1450:4001::200e", true),
        ("::", false),
        ("::1", false),
        ("::127.0.0.1", false),
        ("::ffff:127.0.0.1", false),
        ("::ffff:10.0.0.1", false),
        ("::ffff:8.8.8.8", true),
        ("64:ff9b::a00:1", false),
        ("64:ff9b::808:808", true),
        ("64:ff9b:1::808:808", false),
        ("100::1", false),
        ("2001::1", false),
        ("2001:1ff::1", false),
        ("2001:200::1", true),
        ("2001:db8::1", false),
        ("2002:a00:1::1", false),
        ("2002:7f00:1::1", false),
        ("2002:808:808::1", true),
        ("3fff::1", false),
        ("fc00::1", false),
        ("fd12:3456::1", false),
        ("fe80::1", false),
        ("fec0::1", false),
        ("ff02::1", false),
        ("4000::1", false),
    ] {
        assert_eq!(is_public(ip.parse().unwrap()), public, "{ip}");
    }
}

#[test]
fn wire_format() {
    let request = Request {
        v: 1,
        url: "https://example.com/".into(),
        deny_hosts: vec![],
    };
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        r#"{"v":1,"url":"https://example.com/"}"#
    );
    let page = Reply::Page(Page {
        url: "https://example.com/".into(),
        title: "T".into(),
        text: "x".into(),
        truncated: false,
    });
    assert_eq!(
        serde_json::to_string(&page).unwrap(),
        r#"{"page":{"url":"https://example.com/","title":"T","text":"x","truncated":false}}"#
    );
    let refused = Reply::Refused {
        code: Refusal::AddressRefused,
    };
    assert_eq!(
        serde_json::to_string(&refused).unwrap(),
        r#"{"refused":{"code":"address_refused"}}"#
    );
    for (code, name) in [
        (Refusal::InvalidRequest, "invalid_request"),
        (Refusal::ActionPath, "action_path"),
        (Refusal::DnsFailed, "dns_failed"),
        (Refusal::ContentType, "content_type"),
    ] {
        assert_eq!(serde_json::to_value(code).unwrap(), name);
        assert_eq!(code.code(), name);
    }
    assert!(serde_json::from_str::<Request>(r#"{"v":1,"url":"x","extra":1}"#).is_err());
}
