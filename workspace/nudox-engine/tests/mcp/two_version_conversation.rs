//! Two generations of one package, then the surfaces that need both.
//!
//! `search` and `read` see the current generation. `select_version` changes
//! which generation that is. `diff` is the only surface that sees both at
//! once. A declaration removed between the two versions has to show up as
//! removed, and selecting the old version has to make it searchable again.

use std::time::Duration;

use heart::cost::estimated_text_tokens;
use nudox_engine::mcp::tools::{
    DiffVersionsArgs, DiffVersionsResult, ListVersionsArgs, PackagesArgs, ReadArgs,
    SearchSymbolsArgs, SelectVersionArgs, SelectVersionResult, SymbolFormat,
};
use nudox_engine::mcp::key::PackageLineageDto;
use nudox_engine::mcp::{MarkdownResult, NudoxTools, SymbolKeyDto};
use nudox_engine::wire::DiffVerdict;
use nudox_engine::{
    Engine, EngineConfig, PackageHistorySpec, PackageLoadEvent, PackageVersionSpec,
    ProducerLanguage,
};

const NAME: &str = "generations";
const LINEAGE: &str = "npm:generations";

fn write_version(dir: &std::path::Path, version: &str, body: &str) {
    std::fs::write(
        dir.join("package.json"),
        format!("{{\n  \"name\": \"{NAME}\",\n  \"version\": \"{version}\"\n}}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("index.d.ts"), body).unwrap();
}

#[tokio::test]
async fn two_versions_select_and_diff_in_one_conversation() {
    let older = tempfile::tempdir().expect("older");
    let newer = tempfile::tempdir().expect("newer");
    write_version(
        older.path(),
        "1.0.0",
        "export class Kept {}\nexport function gone(): void;\nexport interface Marker {}\n",
    );
    write_version(
        newer.path(),
        "2.0.0",
        "export class Kept {}\nexport function arrived(): void;\nexport interface Marker {}\n",
    );

    let tools = NudoxTools::new(Engine::start_with_versions(
        EngineConfig::default(),
        vec![PackageHistorySpec {
            name: NAME.to_owned(),
            language: ProducerLanguage::TypeScript,
            versions: vec![
                PackageVersionSpec {
                    root: older.path().to_path_buf(),
                    version: "1.0.0".to_owned(),
                },
                PackageVersionSpec {
                    root: newer.path().to_path_buf(),
                    version: "2.0.0".to_owned(),
                },
            ],
        }],
    ));
    wait_until_loaded(&tools).await;
    wait_until_both_versions(&tools).await;

    let listed = tools
        .do_packages(PackagesArgs { package: None })
        .await
        .expect("packages");
    let row = listed
        .packages
        .iter()
        .find(|row| row.lineage == LINEAGE)
        .unwrap_or_else(|| panic!("packages did not list {LINEAGE}"));
    assert!(
        row.versions.iter().any(|v| v.version == "1.0.0"),
        "1.0.0 missing: {:?}",
        row.versions
    );
    assert!(
        row.versions
            .iter()
            .any(|v| v.version == "2.0.0" && v.is_current),
        "2.0.0 should be the current generation: {:?}",
        row.versions
    );

    let diff = tools
        .do_diff_versions(DiffVersionsArgs {
            package: PackageLineageDto(LINEAGE.to_owned()),
            from_version: "1.0.0".to_owned(),
            to_version: "2.0.0".to_owned(),
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("diff");
    let page = diff.to_markdown();
    let DiffVersionsResult::Diff { diff, .. } = &diff else {
        panic!("both versions are loaded; diff must not be NotLoaded:\n{page}");
    };
    assert!(
        diff.rows.iter().any(|row| {
            &*row.name == "gone" && matches!(row.verdict, DiffVerdict::Removed)
        }),
        "gone was deleted and its key is content-derived; the diff must say Removed:\n{page}"
    );
    assert!(
        diff.rows.iter().any(|row| {
            &*row.name == "arrived" && matches!(row.verdict, DiffVerdict::Added)
        }),
        "arrived is new in 2.0.0; the diff must say Added:\n{page}"
    );
    assert!(
        diff.rows.iter().all(|row| &*row.name != "Kept"),
        "Kept is the same class in both generations, so it must not be a diff row:\n{page}"
    );
    let tokens = estimated_text_tokens(&page);
    assert!(tokens < 2_000, "the diff page is {tokens} tokens:\n{page}");
    let again = tools
        .do_diff_versions(DiffVersionsArgs {
            package: PackageLineageDto(LINEAGE.to_owned()),
            from_version: "1.0.0".to_owned(),
            to_version: "2.0.0".to_owned(),
            limit: Some(50),
            cursor: None,
        })
        .await
        .expect("diff again");
    assert_eq!(page, again.to_markdown(), "the same diff on the next turn must match");

    switch(&tools, "1.0.0").await;
    assert!(search_has(&tools, "gone").await, "1.0.0 must still contain gone");
    assert!(search_has(&tools, "Kept").await, "1.0.0 must still contain Kept");
    assert!(
        !search_has(&tools, "arrived").await,
        "1.0.0 must not contain arrived"
    );

    switch(&tools, "2.0.0").await;
    assert!(search_has(&tools, "Kept").await, "2.0.0 must still contain Kept");
    let arrived = search_named(&tools, "arrived").await;
    let key = SymbolKeyDto::from_wire(&arrived.hit.key);
    let source = read_source(&tools, &key).await;
    assert!(
        source.contains("function arrived"),
        "reading the new function must show its declaration: {source}"
    );
    let messy = format!(" `{}` ", key.0.replace(':', "::").replace('#', ":"));
    let messy_source = read_source(&tools, &SymbolKeyDto(messy)).await;
    assert_eq!(source, messy_source, "paste noise must read the same declaration");
    assert!(
        !search_has(&tools, "gone").await,
        "2.0.0 must not contain gone"
    );
}

async fn switch(tools: &NudoxTools, version: &str) {
    let result = tools
        .do_select_version(SelectVersionArgs {
            package: PackageLineageDto(LINEAGE.to_owned()),
            version: version.to_owned(),
        })
        .await
        .unwrap_or_else(|error| panic!("select {version} failed: {error}"));
    match result {
        SelectVersionResult::Switched {
            version: got, ..
        } => assert_eq!(got, version),
        other => panic!("select {version} must switch, got {other:?}"),
    }
}

async fn search_has(tools: &NudoxTools, name: &str) -> bool {
    let result = tools
        .do_unified_search(SearchSymbolsArgs {
            query: name.to_owned(),
            kinds: None,
            packages: Some(vec![LINEAGE.to_owned()]),
            limit: Some(20),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("search {name} failed: {error}"));
    result.hits.iter().any(|hit| {
        hit.hit
            .display_name
            .rsplit(['.', ':'])
            .next()
            .is_some_and(|leaf| leaf == name)
    })
}

async fn search_named(
    tools: &NudoxTools,
    name: &str,
) -> nudox_engine::mcp::tools::SearchHitDoc {
    let result = tools
        .do_unified_search(SearchSymbolsArgs {
            query: name.to_owned(),
            kinds: Some(vec!["Function".to_owned()]),
            packages: Some(vec![LINEAGE.to_owned()]),
            limit: Some(10),
            cursor: None,
        })
        .await
        .unwrap_or_else(|error| panic!("search {name} failed: {error}"));
    let page = result.to_markdown();
    result
        .hits
        .into_iter()
        .find(|hit| {
            hit.hit
                .display_name
                .rsplit(['.', ':'])
                .next()
                .is_some_and(|leaf| leaf == name)
        })
        .unwrap_or_else(|| panic!("search missed {name}:\n{page}"))
}

async fn read_source(tools: &NudoxTools, key: &SymbolKeyDto) -> String {
    let read = tools
        .do_read(ReadArgs {
            keys: vec![key.clone()].into(),
            format: SymbolFormat::Source,
        })
        .await
        .unwrap_or_else(|error| panic!("read {} failed: {error}", key.0));
    read.symbols[0].source.clone().unwrap_or_default()
}

async fn wait_until_loaded(tools: &NudoxTools) {
    let events = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        match tokio::time::timeout_at(deadline, events.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { .. })) => return,
            Ok(Ok(PackageLoadEvent::LoadFailed { name, error, .. })) => {
                panic!("{name} failed to load: {error}")
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => panic!("package channel closed: {error}"),
            Err(_) => panic!("timed out waiting for the package to load"),
        }
    }
}

async fn wait_until_both_versions(tools: &NudoxTools) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let list = tools
            .do_list_versions(ListVersionsArgs {
                package: PackageLineageDto(LINEAGE.to_owned()),
            })
            .await
            .expect("list_versions");
        if list.versions.len() == 2 {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("timed out with versions {:?}", list.versions);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
