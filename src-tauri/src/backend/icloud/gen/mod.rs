//! Rust generated from `src-tauri/proto/*.proto`, **committed rather than
//! built**.
//!
//! No code generation and no `protoc` run during a normal build. That is a
//! deliberate trade and the reasons are concrete: `prost-build` needs `protoc`
//! on `PATH`, which would become a new prerequisite for every developer
//! machine, for CI, and — worst — for the Android NDK cross-build, in exchange
//! for regenerating a schema that changes approximately never.
//!
//! What keeps committed output honest is [`icloud_gen_is_current`], which
//! regenerates into a temp directory and diffs. It uses `protox` (a protobuf
//! compiler written in Rust) so it needs no `protoc` either, and therefore
//! runs everywhere instead of skipping wherever `protoc` happens to be
//! missing. A guard that skips on the machine where the edit is made is not a
//! guard.
//!
//! Provenance, licence and the exact recovery method: `proto/PROVENANCE.md`.

// The schema is vendored whole — all three files, every message — because it
// is one schema set and M2 (tables, attribute runs, the write path) consumes
// the rest of it. Generating only today's subset would mean regenerating on
// the day M2 starts, with no way to tell a deliberate schema change from a
// widened selection. Cheap to carry: these are plain structs.
#![allow(dead_code)]
// Generated code, not ours to lint.
#![allow(clippy::all)]

pub mod versioned_document {
    include!("versioned_document.rs");
}

pub mod topotext {
    include!("topotext.rs");
}

pub mod crdt {
    include!("crdt.rs");
}

/// Regenerates the committed Rust and fails if it differs — the permanent
/// guard that a `.proto` edit cannot land without its generated counterpart.
///
/// To update after editing a `.proto`:
///
/// ```text
/// JODD_UPDATE_PROTO_GEN=1 cargo test -p jodd --lib icloud_gen_is_current
/// ```
///
/// Writing and checking go through the SAME function on purpose. A separate
/// generator binary would be a second place that knows the codegen settings,
/// and two places that must agree about how output is produced is the defect
/// this test exists to catch, one level up.
#[cfg(test)]
mod drift {
    use std::path::PathBuf;

    /// `versioned_document` first: it is the wrapper every other payload
    /// arrives inside, so this order reads like the decode path does.
    const PROTOS: &[&str] = &["versioned_document.proto", "topotext.proto", "crdt.proto"];

    fn proto_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("proto")
    }

    fn committed_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/backend/icloud/gen")
    }

    /// The one description of how these files are produced.
    fn generate_into(out: &std::path::Path) {
        let dir = proto_dir();
        let files: Vec<PathBuf> = PROTOS.iter().map(|f| dir.join(f)).collect();
        // protox parses `.proto` into the same FileDescriptorSet protoc would
        // emit; prost-build turns that into Rust. No protoc, no network, no
        // build script.
        let fds = protox::compile(&files, [&dir]).expect("the vendored protos must compile");
        let mut cfg = prost_build::Config::new();
        cfg.out_dir(out);
        cfg.compile_fds(fds).expect("prost must generate from the descriptor set");
    }

    /// prost names its output after the proto PACKAGE, snake-cased — not after
    /// the file. Here the two agree for all three (`package CRDT;` lands as
    /// `crdt.rs`, matching `crdt.proto`), which is why this is a plain list
    /// and not a mapping. A future `.proto` whose package and filename differ
    /// would need the pairs spelled out; the test says so by failing with the
    /// names prost actually wrote.
    const OUTPUTS: &[&str] = &["versioned_document.rs", "topotext.rs", "crdt.rs"];

    #[test]
    fn icloud_gen_is_current() {
        let tmp = tempfile::tempdir().expect("tempdir");
        generate_into(tmp.path());

        let updating = std::env::var_os("JODD_UPDATE_PROTO_GEN").is_some();
        let mut stale = Vec::new();

        for name in OUTPUTS {
            // Naming the files prost DID write turns "the mapping is wrong"
            // into a one-read fix instead of a hunt through a deleted tempdir.
            let fresh = std::fs::read_to_string(tmp.path().join(name)).unwrap_or_else(|e| {
                let got: Vec<_> = std::fs::read_dir(tmp.path())
                    .expect("read tempdir")
                    .filter_map(|d| d.ok().map(|d| d.file_name()))
                    .collect();
                panic!("prost did not produce {name}: {e}; it produced {got:?}")
            });
            let target = committed_dir().join(name);

            if updating {
                std::fs::write(&target, &fresh).expect("write regenerated file");
                continue;
            }

            if std::fs::read_to_string(&target).unwrap_or_default() != fresh {
                stale.push(*name);
            }
        }

        // Not an assertion: with the env var set nothing was compared, so
        // there is nothing to assert. Say what happened and stop, rather than
        // reporting a pass this run did not earn.
        assert!(
            !updating,
            "JODD_UPDATE_PROTO_GEN rewrote src/backend/icloud/gen — \
             re-run WITHOUT it to verify the result"
        );
        assert!(
            stale.is_empty(),
            "src/backend/icloud/gen is out of date with proto/ ({}).\n\
             Regenerate with:\n  \
             JODD_UPDATE_PROTO_GEN=1 cargo test -p jodd --lib icloud_gen_is_current",
            stale.join(", ")
        );
    }

    /// The schema is only useful if the M1 read path's own field names survive
    /// generation. Pins the two hops `doc.rs` walks — wrapper to payload, and
    /// payload to visible text — so a regeneration that renamed or dropped
    /// either fails here with the reason, rather than in a decode that quietly
    /// returns nothing.
    #[test]
    fn the_m1_read_path_exists_in_the_generated_types() {
        use super::{topotext, versioned_document};

        let doc = versioned_document::Document::default();
        assert!(doc.version.is_empty(), "Document.version is the repeated wrapper hop");

        let version = versioned_document::Version::default();
        assert!(version.data.is_none(), "Version.data carries the inner document as opaque bytes");

        let s = topotext::String::default();
        assert_eq!(s.string, "", "topotext.String.string is the visible text M1 renders");
    }
}
