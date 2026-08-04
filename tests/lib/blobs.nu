# Object-store / content-addressed blob assertions.

# After Stored, the file:// object store must hold at least min_files CAS objects.
export def assert-blob-store [blobs_dir: string, min_files: int = 2] {
  print $"==> asserting blob object store under ($blobs_dir)"
  let files = if ($blobs_dir | path exists) {
    # find is more portable than glob for deep CAS trees.
    let listed = (^find $blobs_dir -type f | complete)
    if $listed.exit_code == 0 {
      $listed.stdout | lines | where {|l| $l != ""}
    } else {
      []
    }
  } else {
    []
  }
  let count = ($files | length)
  if $count < $min_files {
    print --stderr $"expected content-addressed blob files after Stored, found ($count) under ($blobs_dir)"
    $files | first 40 | each {|f| print --stderr $f }
    error make { msg: $"blob store under-populated: ($count) < ($min_files)" }
  }
  print $"    blob store has ($count) file\(s\)"
  $files | first 12 | each {|f| print $"      ($f)" }
  $count
}
