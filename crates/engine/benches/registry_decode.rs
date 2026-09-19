//! Measures bounded native-registry normalization independently of network I/O.

use std::{hint::black_box, time::Instant};

use backend_engine::registry::{
    EcosystemAdapter, PackageName, RegistryEcosystem, RegistryEndpoint,
};

const ROWS: usize = 4_096;
const WARMUPS: usize = 2;
const SAMPLES: usize = 10;
const SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const SHA512_BASE64: &str =
    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";

fn main() {
    for ecosystem in [
        RegistryEcosystem::Cargo,
        RegistryEcosystem::Npm,
        RegistryEcosystem::Pypi,
        RegistryEcosystem::Maven,
        RegistryEcosystem::Nuget,
        RegistryEcosystem::Golang,
        RegistryEcosystem::Cpp,
    ] {
        let (adapter, body) = fixture(ecosystem);
        for _ in 0..WARMUPS {
            black_box(adapter.decode(black_box(&body)).expect("warmup fixture"));
        }
        let mut elapsed = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let start = Instant::now();
            let releases = adapter.decode(black_box(&body)).expect("benchmark fixture");
            assert_eq!(releases.len(), ROWS);
            black_box(releases);
            elapsed.push(start.elapsed().as_secs_f64());
        }
        elapsed.sort_by(f64::total_cmp);
        let median = elapsed[SAMPLES / 2];
        let p95 = elapsed[SAMPLES - 1];
        println!(
            "{:?}: rows={} median_ms={:.3} p95_ms={:.3} median_rows_per_second={:.0}",
            ecosystem,
            ROWS,
            median * 1_000.0,
            p95 * 1_000.0,
            ROWS as f64 / median,
        );
    }
}

fn fixture(ecosystem: RegistryEcosystem) -> (EcosystemAdapter, Vec<u8>) {
    let origin = "https://registry.example.test";
    let endpoint = RegistryEndpoint::new(ecosystem, origin).expect("endpoint");
    let namespace = match ecosystem {
        RegistryEcosystem::Maven => Some("org.example"),
        RegistryEcosystem::Cpp => Some("stable/example"),
        _ => None,
    };
    let adapter = EcosystemAdapter::new(
        endpoint,
        PackageName::new("bench").expect("package"),
        namespace
            .map(PackageName::new)
            .transpose()
            .expect("namespace"),
    )
    .expect("adapter");
    let rows = (0..ROWS)
        .map(|index| format!("1.0.{index}"))
        .collect::<Vec<_>>();
    let body = match ecosystem {
        RegistryEcosystem::Cargo => rows
            .iter()
            .map(|version| {
                format!(
                    "{{\"name\":\"bench\",\"vers\":\"{version}\",\"cksum\":\"{SHA256}\"}}\n"
                )
            })
            .collect(),
        RegistryEcosystem::Npm => format!(
            "{{\"versions\":{{{}}}}}",
            rows.iter()
                .map(|version| format!(
                    "\"{version}\":{{\"dist\":{{\"integrity\":\"sha512-{SHA512_BASE64}\",\"tarball\":\"{origin}/bench-{version}.tgz\"}}}}"
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        RegistryEcosystem::Pypi => format!(
            "{{\"releases\":{{{}}}}}",
            rows.iter()
                .map(|version| format!(
                    "\"{version}\":[{{\"filename\":\"bench-{version}.tar.gz\",\"packagetype\":\"sdist\",\"url\":\"{origin}/bench-{version}.tar.gz\",\"digests\":{{\"sha256\":\"{SHA256}\"}}}}]"
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        RegistryEcosystem::Maven => format!(
            "<metadata>{}</metadata>",
            rows.iter()
                .map(|version| format!(
                    "<release version=\"{version}\" sha256=\"{SHA256}\" url=\"{origin}/bench-{version}.jar\"/>"
                ))
                .collect::<String>()
        ),
        RegistryEcosystem::Nuget => format!(
            "{{\"items\":[{}]}}",
            rows.iter()
                .map(|version| format!(
                    "{{\"catalogEntry\":{{\"version\":\"{version}\"}},\"packageContent\":\"{origin}/bench-{version}.nupkg\",\"packageHash\":\"{SHA512_BASE64}\"}}"
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        RegistryEcosystem::Golang => rows
            .iter()
            .map(|version| format!("v{version} {SHA256}\n"))
            .collect(),
        RegistryEcosystem::Cpp => format!(
            "{{\"results\":[{}]}}",
            rows.iter()
                .map(|version| format!(
                    "{{\"version\":\"{version}\",\"download_url\":\"{origin}/bench-{version}.tgz\",\"sha256\":\"{SHA256}\"}}"
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
    };
    (adapter, body.into_bytes())
}
