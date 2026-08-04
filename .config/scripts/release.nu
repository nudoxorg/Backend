#!/usr/bin/env nu

def main [] {
    # Detect target (or use RUST_TARGET if available)
    let rust_target = if ($env.RUST_TARGET? != null) {
        $env.RUST_TARGET
    } else {
        # Fallback detection if RUST_TARGET not set
        let os = (uname | get kernel-name | str downcase)
        let arch = (uname | get machine)
        
        if $os == "darwin" {
            if $arch == "arm64" { "aarch64-apple-darwin" } else { "x86_64-apple-darwin" }
        } else if $os == "linux" {
            if $arch == "aarch64" { "aarch64-unknown-linux-gnu" } else { "x86_64-unknown-linux-gnu" }
        } else {
            error make {msg: $"Unsupported OS: ($os)"}
        }
    }
    
    print $"Preparing release for ($rust_target)..."
    
    # Create config for this target
    let goos = if $rust_target =~ "darwin" { "darwin" } else { "linux" }
    let goarch = if $rust_target =~ "aarch64" { "arm64" } else { "amd64" }
    
    open .goreleaser.yaml 
        | str replace "__RUST_TARGET__" $rust_target --all
        | str replace "__GOOS__" $goos 
        | str replace "__GOARCH__" $goarch
        | save -f .goreleaser.local.yaml
    
    # Run goreleaser
    goreleaser release --config .goreleaser.local.yaml --snapshot --clean
    
    # Generate extra checksums
    cd dist
    if (which md5sum | is-empty) == false {
        ls *.{tar.gz,zip} | get name | each { |f| md5sum $f } | save -f MD5.sum
    } else {
        ls *.{tar.gz,zip} | get name | each { |f| md5 -r $f } | save -f MD5.sum
    }
    
    if (which b3sum | is-empty) == false {
         ls *.{tar.gz,zip} | get name | each { |f| b3sum $f } | save -f BLAKE3.sum
    }
    cd ..
    
    # Cleanup
    rm .goreleaser.local.yaml
}
