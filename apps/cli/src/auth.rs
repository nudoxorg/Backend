//! `nudox auth`: sign in with an API key, see the account, sign out.
//!
//! The key is saved where the desktop app and the MCP server read it, so
//! signing in here signs in every surface. CLI commands never count as usage.

use crate::options::{Format, Options};
use backend_account::{ApiKey, KEYS_URL, KeySource, KeyStore, Service, ServiceError};
use backend_present::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};
use serde_json::json;
use std::io::{BufRead as _, IsTerminal as _, Write as _};

pub(super) const fn help_text() -> &'static str {
    "nudox auth — sign in with your Nudox API key\n\n\
     nudox auth login [KEY]   Check a key with api.nudox.org and save it for the app, CLI and MCP\n\
     nudox auth status        Show the signed-in account, plan and this period's usage\n\
     nudox auth logout        Remove the saved key\n\n\
     Create a key at https://nudox.org/dashboard/keys. Without KEY, login reads it from standard \
     input, so it stays out of your shell history: pbpaste | nudox auth login. NUDOX_API_KEY, when \
     set, is used instead of the saved key. Only MCP tool calls count toward usage."
}

pub(super) fn run(words: &[String], options: &Options) -> Result<String, Fault> {
    match words.split_first() {
        Some((verb, rest)) if verb == "login" && rest.len() <= 1 => login(rest.first(), options),
        Some((verb, [])) if verb == "status" => status(options),
        Some((verb, [])) if verb == "logout" => logout(options),
        _ => Err(Fault::usage(
            "auth",
            "use nudox auth login [KEY], nudox auth status or nudox auth logout",
        )),
    }
}

/// The refusal every product command gets in a build that requires an account
/// when no key is saved.
pub(super) fn signed_out() -> Fault {
    refused(format!(
        "sign in first: run `nudox auth login` with a key from {KEYS_URL}, or set NUDOX_API_KEY"
    ))
}

/// Whether a product command may run: always in a debug build, and with a
/// saved (or environment) key in a release build. This reads only the local
/// key; the service is consulted by `auth status` and by MCP tool calls.
pub(super) fn require_signed_in() -> Result<(), Fault> {
    if !backend_account::required() {
        return Ok(());
    }
    match store()?.load() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(signed_out()),
        Err(error) => Err(refused(error.to_string())),
    }
}

fn login(argument: Option<&String>, options: &Options) -> Result<String, Fault> {
    let text = match argument {
        Some(text) => text.clone(),
        None => read_key()?,
    };
    let key = ApiKey::parse(&text).map_err(|error| Fault::usage("key", error.to_string()))?;
    let account = Service::new().account(&key).map_err(|error| match error {
        ServiceError::InvalidKey => refused(format!(
            "api.nudox.org does not accept that key; create a new one at {KEYS_URL}"
        )),
        other => refused(other.to_string()),
    })?;
    store()?
        .save(&key)
        .map_err(|error| refused(error.to_string()))?;
    Ok(match options.format() {
        Format::Json => format!(
            "{}\n",
            json!({ "signed_in": true, "email": account.email, "plan": account.plan_name(), "key": key.redacted() })
        ),
        _ => format!(
            "Signed in as {} ({} plan). The Nudox app, CLI and MCP server now use this key.\n",
            shown_email(&account.email),
            account.plan_name()
        ),
    })
}

fn status(options: &Options) -> Result<String, Fault> {
    let Some((key, source)) = store()?
        .load()
        .map_err(|error| refused(error.to_string()))?
    else {
        return Err(signed_out());
    };
    let service = Service::new();
    let account = service.account(&key);
    let usage = account.as_ref().ok().and_then(|_| service.usage(&key).ok());
    if let Err(ServiceError::InvalidKey) = account {
        return Err(refused(format!(
            "the saved key {} is no longer valid (it may have been revoked or rotated); run \
             `nudox auth login` with a new key from {KEYS_URL}",
            key.redacted()
        )));
    }
    if options.format() == Format::Json {
        let value = match (&account, &usage) {
            (Ok(account), usage) => json!({
                "signed_in": true,
                "key": key.redacted(),
                "source": source_label(&source),
                "email": account.email,
                "plan": account.plan_name(),
                "tool_calls": usage.as_ref().map(|usage| usage.tool_calls),
                "used": usage.as_ref().map(|usage| usage.used),
                "limit": usage.as_ref().and_then(|usage| usage.limit),
            }),
            (Err(error), _) => json!({
                "signed_in": true,
                "key": key.redacted(),
                "source": source_label(&source),
                "unverified": error.to_string(),
            }),
        };
        return Ok(format!("{value}\n"));
    }
    let mut out = String::new();
    match &account {
        Ok(account) => {
            out.push_str(&format!(
                "Signed in as {} ({} plan)\n",
                shown_email(&account.email),
                account.plan_name()
            ));
        }
        Err(error) => out.push_str(&format!("Signed in, but {error}\n")),
    }
    out.push_str(&format!(
        "Key: {} ({})\n",
        key.redacted(),
        source.describe()
    ));
    if let Some(usage) = usage {
        out.push_str(&format!(
            "Usage: {} ({} MCP tool calls)\n",
            usage.summary(),
            usage.tool_calls
        ));
        if usage.over_limit {
            out.push_str(&format!(
                "Limit reached: MCP tool calls are refused until the period resets. Upgrade or \
                 turn on overage at {KEYS_URL}\n"
            ));
        }
    }
    Ok(out)
}

fn logout(options: &Options) -> Result<String, Fault> {
    store()?
        .remove()
        .map_err(|error| refused(error.to_string()))?;
    let environment = std::env::var_os(backend_account::API_KEY_ENV).is_some();
    Ok(match options.format() {
        Format::Json => format!("{}\n", json!({ "signed_in": environment })),
        _ if environment => {
            "Removed the saved key. NUDOX_API_KEY is still set, so this shell stays signed in.\n"
                .to_owned()
        }
        _ => "Signed out. The Nudox app, CLI and MCP server no longer have a key.\n".to_owned(),
    })
}

fn read_key() -> Result<String, Fault> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "Paste your API key from {KEYS_URL}: ");
        let _ = stderr.flush();
    }
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .map_err(|error| Fault::usage("key", format!("cannot read the key: {error}")))?;
    Ok(line)
}

fn store() -> Result<KeyStore, Fault> {
    KeyStore::open().map_err(|error| refused(error.to_string()))
}

fn source_label(source: &KeySource) -> &'static str {
    match source {
        KeySource::File(_) => "saved",
        KeySource::Environment => "environment",
    }
}

fn shown_email(email: &str) -> &str {
    if email.is_empty() {
        "your account"
    } else {
        email
    }
}

fn refused(sentence: String) -> Fault {
    Fault::new(
        FaultSlug::Rejected,
        Operand::Argument("account".to_owned()),
        Cause::new(CauseSlug::Refused, sentence),
        Affordance::None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_auth_verbs_with_the_usage_line() {
        let options = Options::fallback();
        for words in [
            vec![],
            vec!["whoami"],
            vec!["status", "extra"],
            vec!["login", "a", "b"],
        ] {
            let words: Vec<String> = words.into_iter().map(str::to_owned).collect();
            let fault = run(&words, &options).expect_err("usage");
            assert!(fault.slug().is_usage(), "{words:?}");
        }
    }

    #[test]
    fn a_malformed_key_is_refused_before_any_network_call() {
        let fault = run(
            &["login".to_owned(), "sk_live_nope".to_owned()],
            &Options::fallback(),
        )
        .expect_err("bad key");
        assert!(fault.slug().is_usage());
        assert!(fault.cause().sentence().contains("ndx_"));
    }

    #[test]
    fn debug_builds_do_not_require_an_account_by_default() {
        if std::env::var(backend_account::GATE_ENV).is_err() {
            assert!(require_signed_in().is_ok());
        }
    }
}
