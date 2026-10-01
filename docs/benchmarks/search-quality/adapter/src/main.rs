use backend_local_service::search_benchmark;
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    match dispatch(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("search-quality-tantivy-adapter: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(args: Vec<String>) -> Result<(), String> {
    let Some(operation) = args.first().map(String::as_str) else {
        return Err(usage());
    };
    match operation {
        "build" => {
            let corpus = required_path(&args, "--corpus")?;
            let index = required_path(&args, "--index")?;
            emit(search_benchmark::build(&corpus, &index)?)
        }
        "search-cold" => {
            let index_path = required_path(&args, "--index")?;
            let query_path = required_path(&args, "--query-json")?;
            let request = read_json(&query_path)?;
            let index = search_benchmark::open(&index_path)?;
            emit(index.search_request(&request)?)
        }
        "serve" => {
            let index_path = required_path(&args, "--index")?;
            let limit = optional_usize(&args, "--limit")?.unwrap_or(50);
            search_benchmark::serve(&index_path, limit)
        }
        "serve-parallel" => {
            let index_path = required_path(&args, "--index")?;
            let limit = optional_usize(&args, "--limit")?.unwrap_or(50);
            let workers = optional_usize(&args, "--workers")?.unwrap_or(1);
            search_benchmark::serve_parallel(&index_path, limit, workers)
        }
        _ => Err(usage()),
    }
}

fn required_path(args: &[String], flag: &str) -> Result<PathBuf, String> {
    option_value(args, flag)
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing {flag}\n{}", usage()))
}

fn optional_usize(args: &[String], flag: &str) -> Result<Option<usize>, String> {
    option_value(args, flag)
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|error| format!("invalid {flag} value: {error}"))
        })
        .transpose()
}

fn option_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|argument| argument == flag)
        .and_then(|position| args.get(position.saturating_add(1)))
        .map(String::as_str)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("read query {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("decode query JSON: {error}"))
}

fn emit(value: Value) -> Result<(), String> {
    serde_json::to_writer(std::io::stdout().lock(), &value)
        .map_err(|error| format!("write adapter output: {error}"))?;
    println!();
    Ok(())
}

fn usage() -> String {
    "usage: adapter build --corpus FILE --index DIR | adapter serve --index DIR --limit N | adapter serve-parallel --index DIR --workers N --limit N | adapter search-cold --index DIR --query-json FILE --limit N".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_parser_reads_value_after_exact_flag() {
        let args = vec!["serve".to_owned(), "--index".to_owned(), "idx".to_owned()];
        assert_eq!(option_value(&args, "--index"), Some("idx"));
        assert_eq!(option_value(&args, "--limit"), None);
    }

    #[test]
    fn parser_rejects_missing_required_index() {
        let error = required_path(&["serve".to_owned()], "--index")
            .expect_err("missing index should be an error");
        assert!(error.contains("missing --index"));
    }
}
