//! The platform credential store, against the real one on this machine.
//!
//! # Why this is `#[ignore]`d
//!
//! Every other test in this crate uses [`MemoryStore`], for the reasons
//! `account::store`'s module docs give at length: the macOS Keychain prompts,
//! re-prompts on every rebuild because an ad-hoc signature changes the item's
//! ACL, and cannot be unlocked at all on a headless runner. The Linux Secret
//! Service is friendlier — no code-signing involvement — but it still needs a
//! daemon on the session bus, which CI does not have, and it still writes to
//! the developer's real keyring.
//!
//! So this file is opt-in and never runs by default:
//!
//! ```sh
//! cargo test -p nudox-mcp --test account_platform_store -- --ignored --nocapture
//! ```
//!
//! # Why it exists at all
//!
//! Because the alternative was shipping a credential backend whose only
//! evidence was that it compiled. The Linux store was written against `oo7`'s
//! API, and one of its methods — `Collection::delete` — deletes the *entire
//! collection*, i.e. every secret the user owns; the compiler suggests it by
//! name when `delete_item` does not exist. A round trip through the real daemon
//! is the only thing that distinguishes "stores a key" from "compiles".
//!
//! It cleans up after itself: the item it writes is deleted in the same test,
//! under the real service/account attributes, so a run leaves the keyring as it
//! found it.

use nudox_mcp::account::credential::ApiKey;
use nudox_mcp::account::store::{CredentialStoreError, platform_store};

/// A syntactically valid key that is not, and never will be, a real one.
///
/// Never sent anywhere: this test exercises storage, not authorisation.
fn throwaway_key() -> ApiKey {
    ApiKey::parse("ndx_00000000000000000000000000000000test").expect("the fixture key parses")
}

/// Write, read back, delete — against whatever store this platform has.
///
/// The three assertions are the three facts `GateState` branches on, and the
/// middle one is the `cli/cli#13317` bug this module exists to not have: after
/// a delete the answer must be `Ok(None)` ("never signed in"), not `Err`
/// ("could not ask") and not a stale `Ok(Some(_))`.
#[test]
#[ignore = "writes to the developer's real keyring; needs a Secret Service daemon"]
fn the_platform_store_round_trips_a_key() {
    let store = platform_store();
    println!("cost case=platform_store store={}", store.describe());

    // A machine with no daemon answers `Unavailable`, which is a legitimate
    // outcome rather than a failure — say so and stop, instead of asserting
    // something this machine cannot be asked.
    match store.load() {
        Err(CredentialStoreError::Unavailable { detail }) => {
            println!("no credential store on this machine: {detail}");
            return;
        }
        Err(err) => panic!("the store could not be consulted: {err}"),
        Ok(existing) => {
            // Refuse to clobber a real credential. A developer running this on
            // their own machine is exactly who would be signed in.
            assert!(
                existing.is_none(),
                "this machine already has a stored nudox key — sign out first; \
                 this test will not overwrite it"
            );
        }
    }

    let key = throwaway_key();
    store.store(&key).expect("the store accepts a key");

    let loaded = store
        .load()
        .expect("the store answers after a write")
        .expect("a key that was just written is present");
    assert_eq!(
        loaded.fingerprint(),
        key.fingerprint(),
        "the key read back must be the key written"
    );

    store.delete().expect("the store deletes");
    assert!(
        store.load().expect("the store answers after a delete").is_none(),
        "after a delete the answer is `no key`, not an error and not the old key"
    );

    // Deleting an absent key succeeds — the trait says so, and sign-out for an
    // already-signed-out user depends on it.
    store.delete().expect("deleting an absent key succeeds");
}
