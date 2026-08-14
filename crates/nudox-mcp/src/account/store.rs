//! Where the account key lives at rest, and the seam that keeps tests off the
//! login keychain.
//!
//! # The decision, and the two alternatives it beat
//!
//! **Chosen: the platform's own credential store.** On macOS the Keychain
//! (`Security.framework`, a generic-password item under service
//! `org.nudox.api`), reached through the [`security_framework::passwords`]
//! wrapper. On Linux the freedesktop Secret Service over D-Bus — gnome-keyring,
//! KWallet's `ksecretd`, KeePassXC — reached through `oo7`, filing the item
//! under the same service/account pair so the two platforms agree on one
//! identity. See [`SecretServiceStore`] for why that one is D-Bus only.
//!
//! *Rejected: a dotfile.* A plain `~/.config/nudox/credentials.json` is what
//! most tools reach for and it is the single most common way a key escapes: it
//! is in the frame of a screen recording, it is in `git add -A` if the user's
//! home is a dotfiles repo, it is in a tarball'd bug report, and it is readable
//! by every process running as that user. The GitHub CLI's own history is the
//! cautionary tale — `gh` wrote tokens in cleartext to `hosts.yml` until
//! v2.26.0 flipped OS credential storage on by default, and the plaintext
//! fallback it kept for keyring-less environments is *still* an open complaint
//! (<https://github.com/cli/cli/releases/tag/v2.26.0>,
//! <https://github.com/cli/cli/issues/10108>).
//!
//! *Rejected: SQLite.* It buys transactions and indexes for a single row that
//! is written on sign-in and read on launch, and it is still a
//! world-readable-by-this-user file — every objection to the dotfile survives,
//! with a schema on top. The non-secret half of our state *is* a file (see
//! [`super::state::CachedAuthorization`]); it just does not contain a key.
//!
//! # Why this is a trait and not three functions
//!
//! Because the keychain is not reachable from a test, and pretending otherwise
//! would produce exactly the suite doctrine §4 warns about. Three separate,
//! documented facts make this non-negotiable:
//!
//! * A keychain item's ACL is bound to the **Designated Requirement** of the
//!   process that wrote it. An ad-hoc-signed binary — which is what every
//!   `cargo build` produces — gets a new code hash on every rebuild, so the ACL
//!   no longer matches and macOS re-prompts *"wants to use your confidential
//!   information"* on each build, even after the user clicked "Always Allow"
//!   (<https://github.com/openclaw/gogcli/issues/569>).
//! * A headless macOS session cannot unlock the login keychain at all:
//!   `security` calls fail with "User interaction is not allowed", and GitHub
//!   Actions' macOS runners have shipped with a locked or entirely absent
//!   default keychain (<https://github.com/actions/runner-images/issues/4519>,
//!   <https://developer.apple.com/forums/thread/690665>).
//! * A test that pops a system modal is not a test; a test that writes to the
//!   developer's real login keychain is worse.
//!
//! So [`CredentialStore`] is the seam, [`KeychainStore`] is the product
//! implementation, and [`MemoryStore`] is what every test in this crate uses.
//! `tests/account_credential_store.rs` asserts that the store used under
//! `cfg(test)` is never the keychain one.
//!
//! # The failure this module exists to *not* have
//!
//! `gh` conflated "the keyring read failed" with "no token is configured": both
//! returned an empty string, and the HTTP layer then treated an empty token as
//! "this request needs no auth" and sent it unauthenticated
//! (<https://github.com/cli/cli/issues/13317>). That is doctrine §8's
//! `map_err(|_|)` shape wearing a different hat — a real failure converted into
//! a benign-looking value.
//!
//! [`CredentialStore::load`] therefore returns `Result<Option<ApiKey>, _>` and
//! the two negatives are different: `Ok(None)` is *"this machine has never been
//! signed in"* and `Err(_)` is *"the store could not answer"*. They reach
//! [`super::state::GateState`] as [`super::state::GateState::SignedOut`] and
//! [`super::state::GateState::StoreUnavailable`] respectively, and they say
//! different things to the user, because "sign in" is useless advice to someone
//! whose keychain is locked.

use std::sync::Mutex;

use super::credential::ApiKey;

/// The Keychain service name every nudox account item is filed under.
///
/// Reverse-DNS on the *service* the key authenticates against, not on the app,
/// because one day a second nudox app on the same machine must find the same
/// credential rather than prompting for a second one.
pub const KEYCHAIN_SERVICE: &str = "org.nudox.api";

/// The Keychain account name.
///
/// Fixed rather than derived from the user id: the item has to be findable
/// *before* we know who the key belongs to, since finding it is what tells us.
/// A second account slot is what multi-account support would add, and there is
/// exactly one today.
pub const KEYCHAIN_ACCOUNT: &str = "default";

/// The environment variable that overrides stored credentials entirely.
///
/// Modelled on Sublime Text's `License.sublime_license`, which the editor
/// *reads* and never *writes*
/// (<https://www.sublimetext.com/docs/portable_license_keys.html>), and on
/// `GH_TOKEN`, which makes `gh` bypass both the keyring and `hosts.yml`.
///
/// It exists for the two cases the keychain genuinely cannot serve: a headless
/// or CI machine where the keychain will not unlock, and a user who wants the
/// key managed by their own secret manager. When it is set it *wins*, and the
/// UI says so out loud — see [`KeySource::Environment`]. A signed-in state that
/// silently disagrees with the key actually being sent is the same lie as the
/// conflated-`None` bug above, told from the other end.
pub const API_KEY_ENV: &str = "NUDOX_API_KEY";

// ---------------------------------------------------------------------------
// KeySource
// ---------------------------------------------------------------------------

/// Where the credential in use came from.
///
/// Carried alongside the key everywhere it is displayed, because "signed in"
/// means something different depending on the answer: a keychain credential can
/// be signed out from the UI, and an environment one cannot — the user has to
/// edit whatever set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// Read from the platform's own credential store — the macOS Keychain or
    /// the Linux Secret Service — written there by the sign-in flow.
    ///
    /// Still spelled `Keychain` because that is what every caller means by it:
    /// *the store this app can also write to and sign out of*, as opposed to
    /// one the app can only read. [`Self::label`] supplies the platform's own
    /// name for the UI.
    Keychain,
    /// Read from [`API_KEY_ENV`]. Overrides the platform store; not removable
    /// from inside the app.
    Environment,
}

impl KeySource {
    /// How this source should be named in UI, in one short phrase.
    ///
    /// Platform-specific for the stored case: "Keychain" on a Linux desktop
    /// would name something the machine does not have, and the string is shown
    /// to the user in the account panel and the sign-in card.
    pub fn label(self) -> &'static str {
        match self {
            #[cfg(target_os = "macos")]
            Self::Keychain => "Keychain",
            #[cfg(target_os = "linux")]
            Self::Keychain => "keyring",
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            Self::Keychain => "credential store",
            Self::Environment => "NUDOX_API_KEY",
        }
    }
}

// ---------------------------------------------------------------------------
// CredentialStore
// ---------------------------------------------------------------------------

/// Persistent storage for exactly one account key.
///
/// Deliberately narrow: no listing, no enumeration, no iteration. A store that
/// can enumerate credentials is a store something will eventually enumerate
/// into a log line.
pub trait CredentialStore: Send + Sync + 'static {
    /// Read the stored key.
    ///
    /// `Ok(None)` means *no key has ever been stored*. `Err` means *the store
    /// could not be consulted* — a locked keychain, a denied prompt, a headless
    /// session. Collapsing these is the `gh` bug this module's docs open with;
    /// callers must branch on both.
    fn load(&self) -> Result<Option<ApiKey>, CredentialStoreError>;

    /// Write (or replace) the stored key.
    fn store(&self, key: &ApiKey) -> Result<(), CredentialStoreError>;

    /// Remove the stored key. Removing an absent key succeeds.
    fn delete(&self) -> Result<(), CredentialStoreError>;

    /// A short phrase naming this backing store, for diagnostics and for the
    /// sign-in UI's "stored in …" line.
    fn describe(&self) -> &'static str;
}

/// Why a credential store could not answer.
///
/// Exhaustive (doctrine §3's in-workspace rule): a new backing store must break
/// every match so each reader decides what its failures mean, rather than
/// falling into a `_` arm that renders "something went wrong".
#[derive(Debug, thiserror::Error)]
pub enum CredentialStoreError {
    /// The keychain exists but refused: locked, or the user denied the prompt,
    /// or the process's code signature no longer matches the item's ACL.
    ///
    /// This is the *recoverable* one — the user can unlock and retry.
    #[error("the {store} is locked or access was denied: {detail}")]
    AccessDenied {
        /// Which store refused, for the message.
        store: &'static str,
        /// The platform's own text, which is the only thing that distinguishes
        /// "locked" from "denied" from "no keychain at all".
        detail: String,
    },

    /// The item was found but its bytes are not a valid key.
    ///
    /// Distinct from `AccessDenied` because retrying will never help: the
    /// stored value has to be replaced. Reached when a keychain item was
    /// written by a different tool under the same service name, or when a
    /// non-UTF-8 blob is stored.
    #[error("the stored credential is not usable: {reason}")]
    Corrupt {
        /// Why it could not be read back as a key. Never contains the bytes.
        reason: String,
    },

    /// There is no store to consult on this machine.
    ///
    /// Two different facts reach this variant and the `detail` is what tells
    /// them apart: a platform this build has no store *for* at all, and a
    /// platform that has one where nothing is answering — no Secret Service on
    /// the session bus, or no session bus, which is the normal state in a
    /// container or over plain SSH.
    ///
    /// Not an error to hide: in both cases the honest answer is to say which it
    /// was and point at [`API_KEY_ENV`]. The detail is the difference between
    /// "install a keyring" and "this build cannot store keys here".
    #[error("no credential store is available: {detail}")]
    Unavailable {
        /// Which of the two it was, in the platform's own words where there
        /// are any. Never contains a key.
        detail: String,
    },
}

impl CredentialStoreError {
    /// What the user should do about it, distinct from what happened.
    pub fn help(&self) -> &'static str {
        match self {
            Self::AccessDenied { .. } => {
                "Unlock your login keychain and try again. If macOS is asking on every launch, \
                 this build is not code-signed with a stable identity — set NUDOX_API_KEY \
                 instead."
            }
            Self::Corrupt { .. } => "Sign out and sign in again to replace the stored key.",
            Self::Unavailable { .. } => {
                "Set NUDOX_API_KEY in the environment that launches nudox, or start a keyring \
                 service (gnome-keyring, ksecretd, KeePassXC) and sign in again."
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Resolving a credential from all sources
// ---------------------------------------------------------------------------

/// A credential together with where it came from.
#[derive(Clone, Debug)]
pub struct StoredCredential {
    /// The key itself.
    pub key: ApiKey,
    /// Where it was found.
    pub source: KeySource,
}

/// The result of asking every source, in precedence order, for a credential.
///
/// Three outcomes, not two, and not an `Option`: see this module's opening
/// note on the `gh` conflation bug.
#[derive(Debug)]
pub enum CredentialLookup {
    /// A usable credential was found.
    Found(StoredCredential),
    /// Every source was consulted successfully and none held a key.
    Absent,
    /// A source failed. We do **not** know whether a credential exists.
    Unavailable(CredentialStoreError),
}

/// Read the credential from the environment first, then the store.
///
/// # Why the environment wins
///
/// Because setting it is a deliberate act performed by whoever launched the
/// process, and the keychain entry may be months old. The inverse precedence
/// produces the worst outcome available: a user sets `NUDOX_API_KEY` to switch
/// accounts, nothing changes, and there is no signal anywhere that the variable
/// was read and ignored.
///
/// A malformed value in the environment is **not** silently skipped in favour of
/// the keychain, for the same reason: it is reported as
/// [`CredentialLookup::Unavailable`] so the user is told their variable is
/// wrong instead of being quietly signed in as somebody else.
pub fn resolve_credential(store: &dyn CredentialStore) -> CredentialLookup {
    resolve_credential_with_env(std::env::var(API_KEY_ENV), store)
}

/// [`resolve_credential`] with the environment supplied rather than read.
///
/// The split exists because `std::env` is process-global mutable state: a test
/// that sets `NUDOX_API_KEY` to exercise the override races every other test in
/// the binary, and a test that does *not* set it still fails on a developer
/// machine where it happens to be exported. Passing the value in makes the
/// precedence rule a pure function, which is the part worth testing.
pub fn resolve_credential_with_env(
    env: Result<String, std::env::VarError>,
    store: &dyn CredentialStore,
) -> CredentialLookup {
    match env {
        Ok(raw) if !raw.trim().is_empty() => {
            return match ApiKey::parse(&raw) {
                Ok(key) => CredentialLookup::Found(StoredCredential {
                    key,
                    source: KeySource::Environment,
                }),
                Err(e) => CredentialLookup::Unavailable(CredentialStoreError::Corrupt {
                    reason: format!("{API_KEY_ENV} is set but is not a valid key: {e}"),
                }),
            };
        }
        // An unset variable and an empty one mean the same thing here — "the
        // environment is not supplying a key" — and both fall through to the
        // store. `Err(NotUnicode)` deliberately does not: a variable that
        // exists and cannot be read is a fact worth reporting.
        Ok(_) | Err(std::env::VarError::NotPresent) => {}
        Err(std::env::VarError::NotUnicode(_)) => {
            return CredentialLookup::Unavailable(CredentialStoreError::Corrupt {
                reason: format!("{API_KEY_ENV} is set to a value that is not valid Unicode"),
            });
        }
    }

    match store.load() {
        Ok(Some(key)) => CredentialLookup::Found(StoredCredential {
            key,
            source: KeySource::Keychain,
        }),
        Ok(None) => CredentialLookup::Absent,
        Err(e) => CredentialLookup::Unavailable(e),
    }
}

// ---------------------------------------------------------------------------
// MemoryStore
// ---------------------------------------------------------------------------

/// A store that keeps the key in this process and nowhere else.
///
/// This is what every test uses, and it is public rather than `cfg(test)` so
/// the GUI's own test harness — a different crate — can use it too without
/// touching the developer's login keychain.
///
/// It is also the honest answer for a platform with no credential store: losing
/// the key on quit is a worse product than the Keychain and a better one than
/// writing it to a file that the user did not ask us to write.
#[derive(Default)]
pub struct MemoryStore {
    slot: Mutex<Option<ApiKey>>,
}

impl MemoryStore {
    /// An empty store, as a fresh machine would have.
    pub fn empty() -> Self {
        Self::default()
    }

    /// A store already holding a key, for tests that begin signed in.
    pub fn holding(key: ApiKey) -> Self {
        Self {
            slot: Mutex::new(Some(key)),
        }
    }
}

impl CredentialStore for MemoryStore {
    fn load(&self) -> Result<Option<ApiKey>, CredentialStoreError> {
        Ok(self.lock().clone())
    }

    fn store(&self, key: &ApiKey) -> Result<(), CredentialStoreError> {
        *self.lock() = Some(key.clone());
        Ok(())
    }

    fn delete(&self) -> Result<(), CredentialStoreError> {
        *self.lock() = None;
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "in-memory store (not persisted)"
    }
}

impl MemoryStore {
    /// Take the lock, recovering from a poisoned mutex.
    ///
    /// A panic in another thread while holding this lock cannot have left the
    /// `Option` in a torn state — it is one pointer — so the poison flag
    /// carries no information here and propagating it would turn an unrelated
    /// test failure into a cascade.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<ApiKey>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A store that fails every operation, for testing the
/// [`CredentialLookup::Unavailable`] path.
///
/// This exists because that path is the one `gh` got wrong, and a suite with no
/// way to reach it would be a suite that could not have caught the bug.
pub struct FailingStore {
    /// What every call returns.
    detail: String,
}

impl FailingStore {
    /// A store whose every operation reports access denied with `detail`.
    pub fn denying(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    fn err(&self) -> CredentialStoreError {
        CredentialStoreError::AccessDenied {
            store: "test store",
            detail: self.detail.clone(),
        }
    }
}

impl CredentialStore for FailingStore {
    fn load(&self) -> Result<Option<ApiKey>, CredentialStoreError> {
        Err(self.err())
    }
    fn store(&self, _key: &ApiKey) -> Result<(), CredentialStoreError> {
        Err(self.err())
    }
    fn delete(&self) -> Result<(), CredentialStoreError> {
        Err(self.err())
    }
    fn describe(&self) -> &'static str {
        "always-failing store (tests only)"
    }
}

// ---------------------------------------------------------------------------
// SecretServiceStore (Linux)
// ---------------------------------------------------------------------------

/// The freedesktop Secret Service, as a [`CredentialStore`].
///
/// One item in the login collection, tagged with the same
/// [`KEYCHAIN_SERVICE`]/[`KEYCHAIN_ACCOUNT`] pair the macOS item uses, so the
/// two platforms file the credential under one identity rather than two.
/// gnome-keyring, KWallet's `ksecretd` and KeePassXC all serve this interface.
///
/// # D-Bus only, on purpose
///
/// `oo7::Keyring::new()` falls back to oo7's own encrypted *file* keyring when
/// no daemon answers. This uses [`oo7::dbus`] directly so that cannot happen:
/// with no Secret Service on the bus the answer is
/// [`CredentialStoreError::Unavailable`], which routes the user to
/// [`API_KEY_ENV`] exactly as the old no-op store did. The module docs above
/// reject a credential file on disk; a fallback that quietly wrote one would
/// reverse that decision without anyone deciding to.
///
/// # Why every call runs on its own thread
///
/// `CredentialStore` is sync, and [`super::gate::AccountGate::sign_in`] is
/// `async` — so `store()` is called *from inside* a running executor. Blocking
/// that executor's own thread on a future is how you deadlock a single-threaded
/// runtime and how you panic a Tokio one (`Cannot start a runtime from within a
/// runtime`). Handing the future to a fresh thread with its own executor is
/// safe from every context, sync or async, whatever runtime the caller happens
/// to be on — and these calls happen at startup, sign-in and sign-out, so a
/// thread each is not a cost worth optimising away.
#[cfg(target_os = "linux")]
pub struct SecretServiceStore {
    service: String,
    account: String,
}

#[cfg(target_os = "linux")]
impl Default for SecretServiceStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "linux")]
impl SecretServiceStore {
    /// The store for this app's credential.
    pub fn new() -> Self {
        Self {
            service: KEYCHAIN_SERVICE.to_owned(),
            account: KEYCHAIN_ACCOUNT.to_owned(),
        }
    }

    /// The attribute set identifying our one item.
    fn attributes(&self) -> std::collections::HashMap<&str, &str> {
        std::collections::HashMap::from([
            ("service", self.service.as_str()),
            ("account", self.account.as_str()),
        ])
    }

    /// Run a Secret Service future to completion on a dedicated thread.
    ///
    /// See the type's docs for why this is not `block_on` in place.
    fn blocking<T, F>(future: F) -> Result<T, CredentialStoreError>
    where
        F: std::future::Future<Output = Result<T, CredentialStoreError>> + Send + 'static,
        T: Send + 'static,
    {
        std::thread::scope(|scope| {
            scope
                .spawn(|| futures_lite::future::block_on(future))
                .join()
                // A panic inside the D-Bus call is not something to translate
                // into "the store is unavailable" — that would send the user to
                // set an environment variable to work around a bug here.
                .unwrap_or_else(|_| {
                    Err(CredentialStoreError::AccessDenied {
                        store: SECRET_SERVICE_NAME,
                        detail: "the Secret Service call panicked".to_owned(),
                    })
                })
        })
    }

    /// Open the default collection, mapping every failure to say which kind it
    /// is.
    ///
    /// A missing daemon and a locked collection are different facts and get
    /// different answers: "sign in" is useless advice to someone whose keyring
    /// is locked, and "unlock your keyring" is useless to someone who has none.
    async fn collection() -> Result<oo7::dbus::Collection, CredentialStoreError> {
        let service = oo7::dbus::Service::new().await.map_err(|err| {
            // No daemon on the bus, or no session bus at all (a container, a
            // bare TTY, an SSH session). Honest, and the env var still works.
            CredentialStoreError::Unavailable {
                detail: format!("no Secret Service on the session bus: {err}"),
            }
        })?;

        match service.default_collection().await {
            Ok(collection) => Ok(collection),
            Err(err) => Err(CredentialStoreError::AccessDenied {
                store: SECRET_SERVICE_NAME,
                detail: err.to_string(),
            }),
        }
    }
}

/// How the Secret Service names itself in an error message.
#[cfg(target_os = "linux")]
const SECRET_SERVICE_NAME: &str = "Secret Service keyring";

#[cfg(target_os = "linux")]
impl CredentialStore for SecretServiceStore {
    fn load(&self) -> Result<Option<ApiKey>, CredentialStoreError> {
        let attributes: std::collections::HashMap<String, String> = self
            .attributes()
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();

        Self::blocking(async move {
            let collection = Self::collection().await?;
            let items = collection.search_items(&attributes).await.map_err(|err| {
                CredentialStoreError::AccessDenied {
                    store: SECRET_SERVICE_NAME,
                    detail: err.to_string(),
                }
            })?;

            // `Ok(None)` and `Err(_)` mean different things to `GateState` —
            // "never signed in" versus "could not ask" — which is the whole
            // point of this module. An empty search result is the former.
            let Some(item) = items.first() else {
                return Ok(None);
            };

            // Unlocking can prompt. That is correct: the alternative is
            // reporting "not signed in" to someone who is, and sending them to
            // paste a key they already stored.
            // `None`: no window to parent a prompt to. The Secret Service
            // agent still prompts, it is just not modal to our window — the
            // alternative is threading a `WindowIdentifier` from the GUI into a
            // backend crate that has no business knowing windows exist.
            item.unlock(None).await.map_err(|err| {
                CredentialStoreError::AccessDenied {
                    store: SECRET_SERVICE_NAME,
                    detail: err.to_string(),
                }
            })?;

            let secret = item.secret().await.map_err(|err| {
                CredentialStoreError::AccessDenied {
                    store: SECRET_SERVICE_NAME,
                    detail: err.to_string(),
                }
            })?;

            let text = std::str::from_utf8(&secret).map_err(|_| CredentialStoreError::Corrupt {
                reason: "the stored secret is not valid UTF-8".to_owned(),
            })?;

            ApiKey::parse(text)
                .map(Some)
                .map_err(|err| CredentialStoreError::Corrupt {
                    reason: err.to_string(),
                })
        })
    }

    fn store(&self, key: &ApiKey) -> Result<(), CredentialStoreError> {
        let attributes: std::collections::HashMap<String, String> = self
            .attributes()
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        // Exposed once, here, and moved into the future — never logged, never
        // formatted into an error (`ApiKey`'s `Debug` redacts; this is the
        // deliberate exception that has to hand the bytes to the daemon).
        let secret = key.expose().to_owned();

        Self::blocking(async move {
            let collection = Self::collection().await?;
            collection
                // `true` replaces an existing item with the same attributes,
                // which is what signing in again must do — two items under one
                // identity is the state where `load` starts picking arbitrarily.
                .create_item(
                    SECRET_SERVICE_LABEL,
                    &attributes,
                    oo7::Secret::text(&secret),
                    // Replace an existing item with the same attributes, which
                    // is what signing in again must do — two items under one
                    // identity is the state where `load` starts picking
                    // arbitrarily between them.
                    true,
                    None,
                )
                .await
                .map(|_| ())
                .map_err(|err| CredentialStoreError::AccessDenied {
                    store: SECRET_SERVICE_NAME,
                    detail: err.to_string(),
                })
        })
    }

    fn delete(&self) -> Result<(), CredentialStoreError> {
        let attributes: std::collections::HashMap<String, String> = self
            .attributes()
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();

        Self::blocking(async move {
            let collection = Self::collection().await?;
            // Per *item*, never `Collection::delete` — that one deletes the
            // whole collection, i.e. every secret the user owns, and the
            // compiler will happily suggest it as a same-named alternative.
            //
            // Deleting an absent key succeeds (the trait says so): an empty
            // search is a no-op, not an error.
            let items = collection.search_items(&attributes).await.map_err(|err| {
                CredentialStoreError::AccessDenied {
                    store: SECRET_SERVICE_NAME,
                    detail: err.to_string(),
                }
            })?;

            for item in items {
                item.delete(None)
                    .await
                    .map_err(|err| CredentialStoreError::AccessDenied {
                        store: SECRET_SERVICE_NAME,
                        detail: err.to_string(),
                    })?;
            }
            Ok(())
        })
    }

    fn describe(&self) -> &'static str {
        SECRET_SERVICE_NAME
    }
}

/// The human-facing label the keyring UI shows for our item.
///
/// Seen by anyone browsing Seahorse or KWalletManager, so it names the app and
/// the account rather than being an opaque id.
#[cfg(target_os = "linux")]
const SECRET_SERVICE_LABEL: &str = "nudox API key";

// ---------------------------------------------------------------------------
// KeychainStore (macOS)
// ---------------------------------------------------------------------------

/// The macOS Keychain, as a [`CredentialStore`].
///
/// One generic-password item, service [`KEYCHAIN_SERVICE`], account
/// [`KEYCHAIN_ACCOUNT`]. The default accessibility for a generic password added
/// this way is "when unlocked", which is what we want: the key is needed
/// whenever the app runs and never while the machine is locked.
///
/// **Never construct this in a test.** See the module docs — it prompts, and on
/// a headless runner it fails in ways that have nothing to do with the code
/// under test.
#[cfg(target_os = "macos")]
pub struct KeychainStore {
    service: String,
    account: String,
}

/// `errSecItemNotFound`, from `<Security/SecBase.h>`.
///
/// Spelled out rather than imported: `security-framework` does not re-export
/// the `errSec*` constants, and a magic `-25300` at the one comparison that
/// decides "not signed in" versus "the keychain is broken" is exactly the kind
/// of thing that gets read as a typo and changed.
#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

#[cfg(target_os = "macos")]
impl KeychainStore {
    /// The product store: service `org.nudox.api`, account `default`.
    pub fn new() -> Self {
        Self {
            service: KEYCHAIN_SERVICE.to_owned(),
            account: KEYCHAIN_ACCOUNT.to_owned(),
        }
    }

    /// A store under a caller-chosen service name.
    ///
    /// Exists for a manual, opt-in smoke check against a scratch service name
    /// (`NUDOX_KEYCHAIN_SMOKE=1`), never for the automated suite: the automated
    /// suite must not touch the login keychain at all.
    pub fn under_service(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }
}

#[cfg(target_os = "macos")]
impl Default for KeychainStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "macos")]
impl CredentialStore for KeychainStore {
    fn load(&self) -> Result<Option<ApiKey>, CredentialStoreError> {
        match security_framework::passwords::get_generic_password(&self.service, &self.account) {
            Ok(bytes) => {
                let text = String::from_utf8(bytes).map_err(|_| CredentialStoreError::Corrupt {
                    reason: "the keychain item is not valid UTF-8".to_owned(),
                })?;
                let key = ApiKey::parse(&text).map_err(|e| CredentialStoreError::Corrupt {
                    // `ApiKeyError`'s Display never contains the value — see
                    // `credential.rs`. That property is what makes it safe to
                    // put here, where the string ends up in an error a user may
                    // paste into a bug report.
                    reason: format!("the keychain item is not a nudox key: {e}"),
                })?;
                Ok(Some(key))
            }
            // `errSecItemNotFound` is the one status that means "no key
            // stored" rather than "could not read". Everything else — a locked
            // keychain (`errSecInteractionNotAllowed`), a denied prompt
            // (`errSecAuthFailed`), a missing default keychain
            // (`errSecNoSuchKeychain`) — is `AccessDenied`, because treating
            // any of them as "not signed in" is precisely the conflation this
            // module exists to avoid.
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(CredentialStoreError::AccessDenied {
                store: "macOS Keychain",
                detail: e.to_string(),
            }),
        }
    }

    fn store(&self, key: &ApiKey) -> Result<(), CredentialStoreError> {
        security_framework::passwords::set_generic_password(
            &self.service,
            &self.account,
            key.expose().as_bytes(),
        )
        .map_err(|e| CredentialStoreError::AccessDenied {
            store: "macOS Keychain",
            detail: e.to_string(),
        })
    }

    fn delete(&self) -> Result<(), CredentialStoreError> {
        match security_framework::passwords::delete_generic_password(&self.service, &self.account) {
            Ok(()) => Ok(()),
            // Deleting something that is not there is the state the caller
            // wanted. Reporting it as a failure would make "sign out" fail for
            // a user who was already signed out.
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(CredentialStoreError::AccessDenied {
                store: "macOS Keychain",
                detail: e.to_string(),
            }),
        }
    }

    fn describe(&self) -> &'static str {
        "macOS Keychain"
    }
}

/// The platform credential store for this build.
///
/// The Keychain on macOS, the Secret Service on Linux, and a store that reports
/// [`CredentialStoreError::Unavailable`] anywhere else — which is honest, and
/// routes the user to [`API_KEY_ENV`], rather than inventing a dotfile on a
/// platform where we have nothing better.
pub fn platform_store() -> Box<dyn CredentialStore> {
    #[cfg(target_os = "macos")]
    {
        Box::new(KeychainStore::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(SecretServiceStore::new())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Box::new(NoPlatformStore)
    }
}

/// Why a platform with no store of its own cannot answer.
///
/// A named function rather than the variant inline, so the three trait methods
/// cannot drift into saying three different things about one fact.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn no_store_for_this_platform() -> CredentialStoreError {
    CredentialStoreError::Unavailable {
        detail: "this build has no credential store for this platform".to_owned(),
    }
}

/// The stand-in for platforms with no credential store.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub struct NoPlatformStore;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl CredentialStore for NoPlatformStore {
    fn load(&self) -> Result<Option<ApiKey>, CredentialStoreError> {
        Err(no_store_for_this_platform())
    }
    fn store(&self, _key: &ApiKey) -> Result<(), CredentialStoreError> {
        Err(no_store_for_this_platform())
    }
    fn delete(&self) -> Result<(), CredentialStoreError> {
        Err(no_store_for_this_platform())
    }
    fn describe(&self) -> &'static str {
        "no platform credential store"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key() -> ApiKey {
        ApiKey::parse("ndx_2f8c41a9b60d47e3a5710c9fbe2d836a4517").expect("sample parses")
    }

    #[test]
    fn a_memory_store_round_trips_and_deletes() {
        let store = MemoryStore::empty();
        assert!(
            matches!(store.load(), Ok(None)),
            "a fresh store must report absence, not failure"
        );

        store.store(&sample_key()).expect("store succeeds");
        let loaded = store.load().expect("load succeeds").expect("a key is there");
        assert_eq!(
            loaded.expose(),
            sample_key().expose(),
            "the key must come back byte-identical, or the user is signed in as nobody"
        );

        store.delete().expect("delete succeeds");
        assert!(matches!(store.load(), Ok(None)));
        store
            .delete()
            .expect("deleting an absent key is not a failure — sign-out must be idempotent");
    }

    #[test]
    fn absence_and_unavailability_are_different_answers() {
        // The `gh` bug (cli/cli#13317) in one assertion: these two must not be
        // the same value, because "sign in" is the right advice for one and
        // useless for the other.
        let no_env = || Err(std::env::VarError::NotPresent);
        let absent = resolve_credential_with_env(no_env(), &MemoryStore::empty());
        let broken =
            resolve_credential_with_env(no_env(), &FailingStore::denying("keychain is locked"));

        assert!(
            matches!(absent, CredentialLookup::Absent),
            "an empty store means not signed in, got {absent:?}"
        );
        match broken {
            CredentialLookup::Unavailable(CredentialStoreError::AccessDenied {
                detail, ..
            }) => {
                assert!(
                    detail.contains("locked"),
                    "the platform's own text is the only thing that says *why*, got {detail:?}"
                );
            }
            other => panic!("a failing store must not read as 'not signed in', got {other:?}"),
        }
    }

    #[test]
    fn every_store_error_carries_actionable_help() {
        let cases = [
            CredentialStoreError::AccessDenied {
                store: "macOS Keychain",
                detail: "locked".to_owned(),
            },
            CredentialStoreError::Corrupt {
                reason: "not utf-8".to_owned(),
            },
            CredentialStoreError::Unavailable,
        ];
        for case in cases {
            assert!(!case.help().is_empty());
            assert!(
                case.help() != case.to_string(),
                "help must say what to do next, not repeat what happened: {case}"
            );
        }
    }

    #[test]
    fn a_corrupt_store_error_never_carries_the_stored_bytes() {
        // The keychain read path formats `ApiKeyError` into `Corrupt::reason`.
        // That is only safe because `ApiKeyError` never echoes its input; this
        // pins the property at the place that depends on it.
        let e = crate::account::credential::ApiKey::parse("ndx_SECRETBODY\u{2013}XYZ")
            .expect_err("smart dash is rejected");
        let wrapped = CredentialStoreError::Corrupt {
            reason: format!("the keychain item is not a nudox key: {e}"),
        };
        assert!(!wrapped.to_string().contains("SECRETBODY"));
    }

    #[test]
    fn the_environment_wins_over_the_keychain_and_says_so() {
        let store = MemoryStore::holding(sample_key());
        let env_key = "ndx_ffffffffffffffffffffffffffffffffffff";

        match resolve_credential_with_env(Ok(env_key.to_owned()), &store) {
            CredentialLookup::Found(found) => {
                assert_eq!(found.source, KeySource::Environment);
                assert_eq!(
                    found.key.expose(),
                    env_key,
                    "the environment value must be the one actually used, not merely preferred"
                );
            }
            other => panic!("an environment key must be found, got {other:?}"),
        }

        // An unset or blank variable falls through to the store rather than
        // signing the user out.
        for env in [Err(std::env::VarError::NotPresent), Ok(String::new()), Ok("  ".to_owned())] {
            match resolve_credential_with_env(env, &store) {
                CredentialLookup::Found(found) => assert_eq!(found.source, KeySource::Keychain),
                other => panic!("must fall through to the store, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_malformed_environment_key_is_reported_not_silently_skipped() {
        // Falling back to the keychain here would sign the user in as whoever
        // the *old* key belongs to while they believe the variable took effect.
        let store = MemoryStore::holding(sample_key());
        match resolve_credential_with_env(Ok("not-a-nudox-key".to_owned()), &store) {
            CredentialLookup::Unavailable(CredentialStoreError::Corrupt { reason }) => {
                assert!(reason.contains(API_KEY_ENV), "the message must name the variable");
            }
            other => panic!("a bad environment key must be reported, got {other:?}"),
        }
    }

    #[test]
    fn key_source_labels_distinguish_what_the_user_can_change() {
        assert_ne!(
            KeySource::Keychain.label(),
            KeySource::Environment.label(),
            "the UI must be able to say which one is in force — a keychain key \
             can be signed out from the app and an environment one cannot"
        );
    }
}
