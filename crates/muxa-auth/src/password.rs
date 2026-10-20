//! Password hashing.
//!
//! New hashes are argon2 (PHC strings, via `password-auth`). bcrypt hashes
//! (`$2a$`/`$2b$`/`$2y$`, as exported by Supabase Auth and most others) are
//! accepted so imported users keep their passwords; a successful bcrypt check
//! reports [`Verified::NeedsRehash`] so the app can store an argon2 hash.
//!
//! Both run on the blocking pool: they are deliberately slow.

use std::sync::OnceLock;

/// The outcome of [`verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verified {
    /// The password matches.
    Valid,
    /// The password matches, but the stored hash is outdated: store
    /// [`hash`] of the password in its place.
    NeedsRehash,
    /// No match (or no stored hash).
    Invalid,
}

impl Verified {
    /// Whether the password matched.
    #[must_use]
    pub fn is_valid(self) -> bool {
        !matches!(self, Self::Invalid)
    }
}

/// The blocking task was cancelled or panicked.
#[derive(Debug, thiserror::Error)]
#[error("password hashing task failed")]
pub struct PasswordError(#[source] tokio::task::JoinError);

/// Hash `password` for storage.
///
/// # Errors
/// If the blocking task fails.
pub async fn hash(password: String) -> Result<String, PasswordError> {
    tokio::task::spawn_blocking(move || password_auth::generate_hash(password))
        .await
        .map_err(PasswordError)
}

/// Check `password` against `stored`.
///
/// Pass `None` when the account doesn't exist or has no password: a dummy hash
/// is checked anyway, so the response time doesn't reveal which accounts exist.
///
/// # Errors
/// If the blocking task fails.
pub async fn verify(password: String, stored: Option<String>) -> Result<Verified, PasswordError> {
    tokio::task::spawn_blocking(move || verify_blocking(&password, stored.as_deref()))
        .await
        .map_err(PasswordError)
}

fn verify_blocking(password: &str, stored: Option<&str>) -> Verified {
    let Some(stored) = stored.filter(|hash| !hash.is_empty()) else {
        // Result ignored on purpose: this only spends the time a real check would.
        let _timing = password_auth::verify_password(password, dummy_hash());
        return Verified::Invalid;
    };
    if is_bcrypt(stored) {
        return match bcrypt::verify(password, stored) {
            Ok(true) => Verified::NeedsRehash,
            _ => Verified::Invalid,
        };
    }
    match password_auth::verify_password(password, stored) {
        Ok(()) if password_auth::is_hash_obsolete(stored).unwrap_or(true) => Verified::NeedsRehash,
        Ok(()) => Verified::Valid,
        Err(_) => Verified::Invalid,
    }
}

fn is_bcrypt(hash: &str) -> bool {
    ["$2a$", "$2b$", "$2y$"]
        .iter()
        .any(|prefix| hash.starts_with(prefix))
}

fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| password_auth::generate_hash("muxa-auth timing equaliser"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn argon2_round_trip() {
        let stored = hash("correct horse".to_owned()).await.unwrap();
        assert!(stored.starts_with("$argon2"));
        assert_eq!(
            verify("correct horse".to_owned(), Some(stored.clone()))
                .await
                .unwrap(),
            Verified::Valid
        );
        assert_eq!(
            verify("wrong".to_owned(), Some(stored)).await.unwrap(),
            Verified::Invalid
        );
    }

    #[tokio::test]
    async fn bcrypt_is_accepted_and_flagged_for_rehash() {
        let stored = bcrypt::hash("hunter22", 4).unwrap();
        assert_eq!(
            verify("hunter22".to_owned(), Some(stored.clone()))
                .await
                .unwrap(),
            Verified::NeedsRehash
        );
        assert_eq!(
            verify("hunter23".to_owned(), Some(stored)).await.unwrap(),
            Verified::Invalid
        );
    }

    #[tokio::test]
    async fn missing_hash_is_invalid() {
        assert_eq!(
            verify("x".to_owned(), None).await.unwrap(),
            Verified::Invalid
        );
        assert_eq!(
            verify("x".to_owned(), Some(String::new())).await.unwrap(),
            Verified::Invalid
        );
    }
}
