//! OAuth 1.0a primitives for the IBKR Web API: base strings, signatures, the
//! Diffie-Hellman live session token (LST) exchange and DH parameter parsing.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use der::{asn1::UintRef, Reader as _};
use hmac::{Hmac, Mac};
use num_bigint::BigUint;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use rand::{rngs::OsRng, RngCore};
use ring::{
    rand::SystemRandom,
    signature::{RsaKeyPair, RSA_PKCS1_SHA256},
};
use sha1::Sha1;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::IbkrError;

/// RFC 3986 unreserved characters stay; everything else is percent-encoded.
const RFC3986: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Percent-encodes into a buffer reserved for the worst case (3 bytes per
/// byte), so no reallocation leaves partial copies of the input behind.
pub(crate) fn encode(value: &str) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(value.len() * 3));
    for part in utf8_percent_encode(value, RFC3986) {
        out.push_str(part);
    }
    out
}

/// `prepend + METHOD & enc(url) & enc(sorted k=v joined by &)`.
///
/// `url` carries no query: query parameters belong in `params`. The `k=v`
/// pairs are not encoded one by one; the joined string is encoded once, as in
/// IBKR's own example. `realm` and `oauth_signature` never belong in `params`.
pub(crate) fn base_string(
    prepend: &str,
    method: &str,
    url: &str,
    params: &[(&str, &str)],
) -> Zeroizing<String> {
    let mut pairs: Vec<_> = params.to_vec();
    pairs.sort_unstable();
    let joined_len: usize = pairs.iter().map(|(k, v)| k.len() + v.len() + 2).sum();
    let mut joined = Zeroizing::new(String::with_capacity(joined_len));
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            joined.push('&');
        }
        joined.push_str(key);
        joined.push('=');
        joined.push_str(value);
    }
    let (url, joined) = (encode(url), encode(&joined));
    let mut base = Zeroizing::new(String::with_capacity(
        prepend.len() + method.len() + url.len() + joined.len() + 2,
    ));
    for part in [prepend, method, "&", &url, "&", &joined] {
        base.push_str(part);
    }
    base
}

/// `OAuth realm="..", k="v", ..` with the pairs sorted by key.
pub(crate) fn authorization_header(realm: &str, params: &[(&str, &str)]) -> String {
    let mut pairs: Vec<_> = params.to_vec();
    pairs.sort_unstable();
    let mut header = format!("OAuth realm=\"{realm}\"");
    for (key, value) in pairs {
        header.push_str(&format!(", {key}=\"{value}\""));
    }
    header
}

/// Percent-encoded base64 RSA-SHA256 (PKCS#1 v1.5) signature, for the LST request.
pub(crate) fn rsa_sha256_signature(key: &RsaKeyPair, base: &str) -> Result<String, IbkrError> {
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        base.as_bytes(),
        &mut signature,
    )
    .map_err(|_| IbkrError::Handshake("signing the LST request failed"))?;
    Ok(encode(&B64.encode(signature)).to_string())
}

/// DER body of the first PEM block with exactly this label.
pub(crate) fn pem_body(pem: &str, label: &str) -> Option<Zeroizing<Vec<u8>>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = pem.find(&begin)? + begin.len();
    let body = &pem[start..start + pem[start..].find(&end)?];
    let text: Zeroizing<String> =
        Zeroizing::new(body.chars().filter(|c| !c.is_whitespace()).collect());
    B64.decode(text.as_bytes()).ok().map(Zeroizing::new)
}

/// Percent-encoded base64 HMAC-SHA256 signature keyed by the LST bytes.
pub(crate) fn hmac_sha256_signature(lst: &[u8], base: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(lst).expect("HMAC takes any key length");
    mac.update(base.as_bytes());
    encode(&B64.encode(mac.finalize().into_bytes())).to_string()
}

/// Random value of `bytes` length as lowercase hex (nonces).
pub(crate) fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0; bytes];
    OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

/// A fresh 256-bit DH private value `a`.
// ponytail: neither num-bigint values (`a`, K) nor ring's `RsaKeyPair` are
// zeroized: num-bigint has no Zeroize and ring keeps the key internally. `a`
// and K live for one mint; zeroizing them needs a bigint crate with Zeroize.
pub(crate) fn dh_private() -> BigUint {
    let mut buf = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(buf.as_mut());
    BigUint::from_bytes_be(buf.as_ref())
}

/// Java `BigInteger.toByteArray()` encoding of a positive integer: minimal
/// big-endian bytes, with a leading 0x00 when the top bit is set. IBKR keys the
/// LST HMAC with this; dropping the sign byte fails validation for about half
/// of all K.
pub(crate) fn java_bytes(value: &BigUint) -> Zeroizing<Vec<u8>> {
    let minimal = Zeroizing::new(value.to_bytes_be());
    // Sized up front so no unzeroed copy is left behind by a reallocation.
    let mut bytes = Zeroizing::new(Vec::with_capacity(minimal.len() + 1));
    if minimal[0] & 0x80 != 0 {
        bytes.push(0);
    }
    bytes.extend_from_slice(&minimal);
    bytes
}

/// LST bytes = HMAC-SHA1(K, secret) with K = B^a mod p.
pub(crate) fn live_session_token(
    response_hex: &str,
    a: &BigUint,
    prime: &BigUint,
    secret: &[u8],
) -> Result<Zeroizing<Vec<u8>>, IbkrError> {
    let b = BigUint::parse_bytes(response_hex.as_bytes(), 16)
        .ok_or(IbkrError::Handshake("DH response is not hex"))?;
    // Reject the degenerate values 0, 1 and p-1 (and anything >= p).
    let one = BigUint::from(1u8);
    if b <= one || b >= prime - &one {
        return Err(IbkrError::Handshake("DH response out of range"));
    }
    let k = b.modpow(a, prime);
    let mut mac = Hmac::<Sha1>::new_from_slice(&java_bytes(&k)).expect("HMAC takes any key length");
    mac.update(secret);
    Ok(Zeroizing::new(mac.finalize().into_bytes().to_vec()))
}

/// Constant-time check that `hex(HMAC-SHA1(lst, consumer_key)) == expected_hex`.
pub(crate) fn lst_signature_matches(lst: &[u8], consumer_key: &str, expected_hex: &str) -> bool {
    let Ok(expected) = hex::decode(expected_hex) else {
        return false;
    };
    let mut mac = Hmac::<Sha1>::new_from_slice(lst).expect("HMAC takes any key length");
    mac.update(consumer_key.as_bytes());
    mac.verify_slice(&expected).is_ok()
}

/// Prime `p` from a PKCS#3 `DH PARAMETERS` PEM. The generator must be 2,
/// which IBKR uses and the challenge is computed with.
pub(crate) fn dh_prime(pem: &str) -> Result<BigUint, IbkrError> {
    const INVALID: IbkrError = IbkrError::Credential("DH parameters");
    let document = pem_body(pem, "DH PARAMETERS").ok_or(INVALID)?;
    let mut reader = der::SliceReader::new(&document).map_err(|_| INVALID)?;
    let (prime, generator) = reader
        .sequence(|nested| {
            let prime = BigUint::from_bytes_be(nested.decode::<UintRef<'_>>()?.as_bytes());
            let generator = BigUint::from_bytes_be(nested.decode::<UintRef<'_>>()?.as_bytes());
            // Optional privateValueLength.
            nested.decode::<Option<UintRef<'_>>>()?;
            Ok((prime, generator))
        })
        .and_then(|value| reader.finish(value))
        .map_err(|_| INVALID)?;
    if generator != BigUint::from(2u8) || !(2048..=8192).contains(&prime.bits()) {
        return Err(INVALID);
    }
    Ok(prime)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use der::{asn1::Uint, Encode as _};

    /// RFC 3526 group 14 (2048-bit MODP) prime: public, generator 2.
    pub(crate) const RFC3526_PRIME: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA051015728E5A8AACAA68FFFFFFFFFFFFFFFF";

    pub(crate) fn prime() -> BigUint {
        BigUint::parse_bytes(RFC3526_PRIME.as_bytes(), 16).unwrap()
    }

    /// PKCS#3 PEM built in-test, so no key material is committed.
    pub(crate) fn dhparam_pem(prime: &BigUint, generator: u8, extra: Option<u8>) -> String {
        let mut body = Vec::new();
        Uint::new(&prime.to_bytes_be())
            .unwrap()
            .encode_to_vec(&mut body)
            .unwrap();
        Uint::new(&[generator])
            .unwrap()
            .encode_to_vec(&mut body)
            .unwrap();
        if let Some(extra) = extra {
            Uint::new(&[extra])
                .unwrap()
                .encode_to_vec(&mut body)
                .unwrap();
        }
        let mut der = Vec::new();
        der::Header::new(
            der::Tag::Sequence,
            der::Length::try_from(body.len()).unwrap(),
        )
        .unwrap()
        .encode_to_vec(&mut der)
        .unwrap();
        der.extend_from_slice(&body);
        format!(
            "-----BEGIN DH PARAMETERS-----\n{}\n-----END DH PARAMETERS-----\n",
            B64.encode(der)
        )
    }

    const TEST_LST: &str = "YBWbLw+9RYP2nWrPQHxHZkBb1aM=";

    #[test]
    fn lst_validation_matches_ibkr_vector() {
        let lst = B64.decode(TEST_LST).unwrap();
        assert!(lst_signature_matches(
            &lst,
            "TESTCONS",
            "543c55477d6cbb0e792d1e4f8111cec7305ba3f4"
        ));
        assert!(!lst_signature_matches(
            &lst,
            "TESTCONS",
            "543c55477d6cbb0e792d1e4f8111cec7305ba3f5"
        ));
        assert!(!lst_signature_matches(
            &lst,
            "OTHERKEY",
            "543c55477d6cbb0e792d1e4f8111cec7305ba3f4"
        ));
        assert!(!lst_signature_matches(&lst, "TESTCONS", "not hex"));
    }

    #[test]
    fn base_string_and_request_signature_match_ibkr_vector() {
        let base = base_string(
            "",
            "GET",
            "http://localhost:12345/tradingapi/v1/marketdata/snapshot",
            &[
                ("oauth_token", "6f531f8fd316915af53f"),
                ("oauth_timestamp", "1473795686"),
                ("oauth_signature_method", "HMAC-SHA256"),
                ("oauth_nonce", "aecef17086308940e861"),
                ("oauth_consumer_key", "TESTCONS"),
                ("conid", "8314"),
            ],
        );
        assert_eq!(
            base.as_str(),
            "GET&http%3A%2F%2Flocalhost%3A12345%2Ftradingapi%2Fv1%2Fmarketdata%2Fsnapshot&conid%3D8314%26oauth_consumer_key%3DTESTCONS%26oauth_nonce%3Daecef17086308940e861%26oauth_signature_method%3DHMAC-SHA256%26oauth_timestamp%3D1473795686%26oauth_token%3D6f531f8fd316915af53f"
        );
        let lst = B64.decode(TEST_LST).unwrap();
        assert_eq!(
            hmac_sha256_signature(&lst, &base),
            encode("+BdIuZDNooYZAbO9RZUCTC5F/3HjFOb04Tu4crpi0v8=").as_str()
        );
        assert_eq!(
            encode("+BdI/x=").as_str(),
            "%2BBdI%2Fx%3D",
            "signature characters are percent-encoded"
        );
    }

    #[test]
    fn lst_base_string_prepends_the_secret_without_separator() {
        let base = base_string(
            "a1b2",
            "POST",
            "https://api.ibkr.com/v1/api/oauth/live_session_token",
            &[("oauth_token", "tok"), ("diffie_hellman_challenge", "ff")],
        );
        assert_eq!(
            base.as_str(),
            "a1b2POST&https%3A%2F%2Fapi.ibkr.com%2Fv1%2Fapi%2Foauth%2Flive_session_token&diffie_hellman_challenge%3Dff%26oauth_token%3Dtok"
        );
    }

    #[test]
    fn authorization_header_puts_realm_first_and_sorts_pairs() {
        assert_eq!(
            authorization_header("limited_poa", &[("oauth_token", "t"), ("oauth_nonce", "n")]),
            r#"OAuth realm="limited_poa", oauth_nonce="n", oauth_token="t""#
        );
    }

    #[test]
    fn java_bytes_adds_a_sign_byte_only_when_the_top_bit_is_set() {
        assert_eq!(*java_bytes(&BigUint::from(0xffu8)), vec![0, 0xff]);
        assert_eq!(*java_bytes(&BigUint::from(0x7fu8)), vec![0x7f]);
        assert_eq!(*java_bytes(&BigUint::from(0x80_00u16)), vec![0, 0x80, 0]);
        assert_eq!(*java_bytes(&BigUint::from(0x01_00u16)), vec![1, 0]);
    }

    #[test]
    fn lst_uses_the_sign_byte_for_a_k_with_the_top_bit_set() {
        // p = 2^127 - 1 (prime), a = 1, so K = B; pick B with the top byte >= 0x80.
        let p = (BigUint::from(1u8) << 127usize) - BigUint::from(1u8);
        let secret = b"secret";
        for hex in [
            "7f0102030405060708090a0b0c0d0e",
            "ff0102030405060708090a0b0c0d0e",
        ] {
            let k = BigUint::parse_bytes(hex.as_bytes(), 16).unwrap();
            let lst =
                live_session_token(&k.to_str_radix(16), &BigUint::from(1u8), &p, secret).unwrap();
            let mut expected = hex::decode(hex).unwrap();
            if hex.starts_with("ff") {
                expected.insert(0, 0);
            }
            let mut mac = Hmac::<Sha1>::new_from_slice(&expected).unwrap();
            mac.update(secret);
            assert_eq!(*lst, mac.finalize().into_bytes().to_vec());
        }
    }

    #[test]
    fn dh_exchange_agrees_on_both_sides() {
        let p = prime();
        let two = BigUint::from(2u8);
        let (a, b) = (dh_private(), dh_private());
        let (big_a, big_b) = (two.modpow(&a, &p), two.modpow(&b, &p));
        let client = live_session_token(&big_b.to_str_radix(16), &a, &p, b"s").unwrap();
        let server = live_session_token(&big_a.to_str_radix(16), &b, &p, b"s").unwrap();
        assert_eq!(client, server);
    }

    #[test]
    fn rejects_degenerate_dh_responses() {
        let p = prime();
        let a = dh_private();
        for bad in [
            "0".to_string(),
            "1".to_string(),
            (&p - BigUint::from(1u8)).to_str_radix(16),
            p.to_str_radix(16),
            "zz".to_string(),
        ] {
            assert!(
                matches!(
                    live_session_token(&bad, &a, &p, b"s"),
                    Err(IbkrError::Handshake(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn dhparam_round_trip() {
        let p = prime();
        assert_eq!(dh_prime(&dhparam_pem(&p, 2, None)).unwrap(), p);
        assert_eq!(dh_prime(&dhparam_pem(&p, 2, Some(224))).unwrap(), p);
        for bad in [
            dhparam_pem(&p, 5, None),
            dhparam_pem(&BigUint::from(23u8), 2, None),
            dhparam_pem(&p, 2, None).replace("DH PARAMETERS", "PRIVATE KEY"),
            "garbage".to_string(),
        ] {
            assert!(matches!(dh_prime(&bad), Err(IbkrError::Credential(_))));
        }
    }
}
