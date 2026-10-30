use keyring::Entry;
use serde::{Deserialize, Serialize};

const SERVICE: &str = "capsule";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Apple,
    Navidrome,
    Lastfm,
    Spotify,
}

fn account_for(slot: Slot) -> &'static str {
    match slot {
        Slot::Apple => "apple-music-tokens",
        Slot::Navidrome => "navidrome-credentials",
        Slot::Lastfm => "lastfm-session",
        Slot::Spotify => "spotify-session",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("credential store unavailable: {0}")]
    Store(#[from] keyring::Error),
    #[error("stored credential was malformed: {0}")]
    Malformed(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tokens {
    pub developer_token: String,
    pub music_user_token: String,
    #[serde(default)]
    pub storefront: String,
}

impl Tokens {
    pub fn is_complete(&self) -> bool {
        !self.developer_token.trim().is_empty() && !self.music_user_token.trim().is_empty()
    }
}

fn entry_for(slot: Slot) -> Result<Entry, AuthError> {
    Ok(Entry::new(SERVICE, account_for(slot))?)
}

fn entry() -> Result<Entry, AuthError> {
    entry_for(Slot::Apple)
}

pub fn load() -> Result<Option<Tokens>, AuthError> {
    match entry()?.get_password() {
        Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(AuthError::Store(e)),
    }
}

/// Navidrome's Subsonic token auth computes `md5(password + salt)` per request,
/// so unlike the Apple path we must hold the password itself rather than a
/// token derived from it. It lives in the OS credential store - Credential
/// Manager on Windows, Secret Service on Linux - never in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NavidromeCredentials {
    pub password: String,
}

pub fn save_navidrome(creds: &NavidromeCredentials) -> Result<(), AuthError> {
    let json = serde_json::to_string(creds)?;
    entry_for(Slot::Navidrome)?.set_password(&json)?;
    Ok(())
}

pub fn load_navidrome() -> Result<Option<NavidromeCredentials>, AuthError> {
    match entry_for(Slot::Navidrome)?.get_password() {
        Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(AuthError::Store(e)),
    }
}

pub fn save_lastfm(session: &crate::lastfm::Session) -> Result<(), AuthError> {
    let json = serde_json::to_string(session)?;
    entry_for(Slot::Lastfm)?.set_password(&json)?;
    Ok(())
}

pub fn load_lastfm() -> Result<Option<crate::lastfm::Session>, AuthError> {
    match entry_for(Slot::Lastfm)?.get_password() {
        Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(AuthError::Store(e)),
    }
}

pub fn clear_lastfm() -> Result<(), AuthError> {
    match entry_for(Slot::Lastfm)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(AuthError::Store(e)),
    }
}

pub fn save_spotify(session: &crate::spotify::Session) -> Result<(), AuthError> {
    let json = serde_json::to_string(session)?;
    entry_for(Slot::Spotify)?.set_password(&json)?;
    Ok(())
}

pub fn load_spotify() -> Result<Option<crate::spotify::Session>, AuthError> {
    match entry_for(Slot::Spotify)?.get_password() {
        Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(AuthError::Store(e)),
    }
}

pub fn clear_spotify() -> Result<(), AuthError> {
    match entry_for(Slot::Spotify)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(AuthError::Store(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_tokens_are_rejected() {
        let t = Tokens {
            developer_token: "abc".into(),
            music_user_token: "   ".into(),
            storefront: "us".into(),
        };
        assert!(!t.is_complete());
    }

    #[test]
    fn complete_tokens_are_accepted() {
        let t = Tokens {
            developer_token: "abc".into(),
            music_user_token: "def".into(),
            storefront: "tr".into(),
        };
        assert!(t.is_complete());
    }

    #[test]
    fn tokens_round_trip_through_json() {
        let t = Tokens {
            developer_token: "d".into(),
            music_user_token: "m".into(),
            storefront: "tr".into(),
        };
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Tokens>(&s).unwrap(), t);
    }

    #[test]
    fn navidrome_credentials_round_trip_through_json() {
        let creds = NavidromeCredentials {
            password: "hunter2".into(),
        };
        let json = serde_json::to_string(&creds).unwrap();
        assert_eq!(
            serde_json::from_str::<NavidromeCredentials>(&json).unwrap(),
            creds
        );
    }

    #[test]
    fn apple_and_navidrome_use_distinct_accounts() {
        assert_ne!(account_for(Slot::Apple), account_for(Slot::Navidrome));
        assert_ne!(account_for(Slot::Spotify), account_for(Slot::Apple));
        assert_ne!(account_for(Slot::Spotify), account_for(Slot::Navidrome));
        assert_ne!(account_for(Slot::Spotify), account_for(Slot::Lastfm));
    }

    #[test]
    fn the_credential_store_persists_to_disk() {
        let persistence = keyring::default::default_credential_builder().persistence();
        assert!(
            matches!(
                persistence,
                keyring::credential::CredentialPersistence::UntilDelete
            ),
            "keyring has no disk-backed store for this platform; credentials will \
             not survive, add the backend feature in Cargo.toml"
        );
    }

    #[test]
    fn storefront_defaults_when_absent_from_stored_json() {
        let t: Tokens =
            serde_json::from_str(r#"{"developer_token":"d","music_user_token":"m"}"#).unwrap();
        assert_eq!(t.storefront, "");
        assert!(t.is_complete());
    }
}
