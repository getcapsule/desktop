use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
pub const REDIRECT_URI: &str = "http://127.0.0.1:43821/callback";

pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn valid_client_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit())
}

pub fn random_secret() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "Could not obtain secure randomness".to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Only accept our exact callback path and unambiguous state/code pairs.
pub fn callback_code(target: &str, expected_state: &str) -> Result<Option<String>, String> {
    if !target.starts_with('/') || target.starts_with("//") {
        return Err("Invalid Spotify callback".into());
    }
    let parsed = url::Url::parse(&format!("http://127.0.0.1:43821{target}"))
        .map_err(|_| "Invalid Spotify callback")?;
    if parsed.path() != "/callback" {
        return Ok(None);
    }
    let pairs: Vec<_> = parsed.query_pairs().collect();
    let values = |name: &str| {
        pairs
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_ref())
            .collect::<Vec<_>>()
    };
    let states = values("state");
    if states.len() != 1 || states[0] != expected_state {
        return Err("Spotify sign-in state did not match. Try connecting again.".into());
    }
    if !values("error").is_empty() {
        return Err("Spotify sign-in was cancelled.".into());
    }
    let codes = values("code");
    if codes.len() != 1 || codes[0].is_empty() {
        return Err("Spotify did not return an authorization code.".into());
    }
    Ok(Some(codes[0].to_string()))
}

pub fn authorize_url(client_id: &str, state: &str, verifier: &str) -> String {
    let mut url = url::Url::parse("https://accounts.spotify.com/authorize").expect("static URL");
    url.query_pairs_mut().extend_pairs([
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT_URI),
        ("state", state),
        ("code_challenge_method", "S256"),
        ("code_challenge", &challenge(verifier)),
        (
            "scope",
            "user-read-private playlist-read-private playlist-read-collaborative",
        ),
        ("show_dialog", "true"),
    ]);
    url.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_authorization_never_requests_playback_permissions() {
        let url = url::Url::parse(&authorize_url("client", "state", "verifier")).unwrap();
        let scope = url
            .query_pairs()
            .find(|(key, _)| key == "scope")
            .unwrap()
            .1
            .into_owned();
        assert_eq!(
            scope,
            "user-read-private playlist-read-private playlist-read-collaborative"
        );
    }
    #[test]
    fn pkce_matches_rfc7636_reference() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
    #[test]
    fn callback_requires_matching_unique_state_and_code() {
        assert_eq!(
            callback_code("/callback?code=abc%2B123&state=ours", "ours").unwrap(),
            Some("abc+123".into())
        );
        assert!(callback_code("/callback?code=stolen&state=theirs", "ours").is_err());
        assert!(callback_code("/callback?code=abc", "ours").is_err());
        assert!(callback_code("/callback?code=abc&state=ours&state=evil", "ours").is_err());
        assert!(callback_code("/callback?code=abc&code=def&state=ours", "ours").is_err());
        assert!(
            callback_code("/callback?state=ours&error=access_denied", "ours")
                .unwrap_err()
                .contains("cancelled")
        );
        assert_eq!(callback_code("/favicon.ico", "ours").unwrap(), None);
        assert!(
            callback_code("https://evil.example/callback?code=abc&state=ours", "ours").is_err()
        );
    }
    #[test]
    fn client_id_cannot_contain_url_or_shell_syntax() {
        assert!(valid_client_id("0123456789abcdef0123456789ABCDEF"));
        assert!(!valid_client_id(""));
        assert!(!valid_client_id("a&redirect_uri=https://evil.example"));
        assert!(!valid_client_id("0123456789abcdef0123456789abcdef "));
    }
}
